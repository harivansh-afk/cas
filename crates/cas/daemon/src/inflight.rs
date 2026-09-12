//! Bounded shared metadata retained by QEMU across a daemon replacement.
//!
//! This module does not establish exclusive image ownership or durability.
//! The caller must exclude the previous writer before attaching/reconciling,
//! serialize admission, and hold the image completion lock through publication.

mod layout;
mod mapping;
mod recovery;

#[cfg(test)]
mod tests;

use std::fs::File;
use std::io;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};

use cas_core::{BLOCK_SIZE, MAX_REQUEST_BYTES};
use vhost::vhost_user::message::VhostUserInflight;

use layout::{
    ACTIVE, Descriptor, EMPTY, Header, MAGIC, PAGE, PREPARED, Queue, REJECTED, Slot, VERSION,
};
use mapping::Mapping;

pub use recovery::Replay;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Geometry {
    queues: u16,
    queue_size: u16,
}

impl Geometry {
    pub fn new(queues: u16, queue_size: u16) -> io::Result<Self> {
        if !(1..=4).contains(&queues) || !queue_size.is_power_of_two() || queue_size > 256 {
            return Err(invalid("unsupported inflight queue geometry"));
        }
        Ok(Self { queues, queue_size })
    }

    fn stride(self) -> usize {
        (size_of::<Queue>() + usize::from(self.queue_size) * size_of::<Descriptor>())
            .next_multiple_of(64)
    }

    fn trailer(self) -> usize {
        (self.stride() * usize::from(self.queues)).next_multiple_of(PAGE)
    }

    pub fn bytes(self) -> usize {
        (self.trailer()
            + PAGE
            + usize::from(self.queues) * usize::from(self.queue_size) * size_of::<Slot>())
        .next_multiple_of(PAGE)
    }

    pub fn message(self) -> VhostUserInflight {
        VhostUserInflight {
            mmap_size: self.bytes() as u64,
            mmap_offset: 0,
            num_queues: self.queues,
            queue_size: self.queue_size,
        }
    }

    fn check(self, queue: u16, head: u16) -> io::Result<()> {
        if queue >= self.queues || head >= self.queue_size {
            return Err(invalid("inflight queue or head out of bounds"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Identity {
    pub store: [u8; 16],
    pub image: [u8; 16],
    pub epoch: u64,
    pub attachment: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum Kind {
    Read = 1,
    Write = 2,
    Zero = 3,
    Flush = 4,
    Protocol = 5,
}

impl TryFrom<u16> for Kind {
    type Error = io::Error;

    fn try_from(value: u16) -> io::Result<Self> {
        match value {
            1 => Ok(Self::Read),
            2 => Ok(Self::Write),
            3 => Ok(Self::Zero),
            4 => Ok(Self::Flush),
            5 => Ok(Self::Protocol),
            _ => Err(invalid("unknown inflight request kind")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Request {
    pub kind: Kind,
    pub queue: u16,
    pub head: u16,
    pub available: u16,
    pub offset: u64,
    pub length: u64,
}

impl Request {
    fn mutates(self) -> bool {
        matches!(self.kind, Kind::Write | Kind::Zero) && self.length != 0
    }

    fn validate(self, image_bytes: u64) -> io::Result<()> {
        match self.kind {
            Kind::Flush | Kind::Protocol if self.offset == 0 && self.length == 0 => Ok(()),
            Kind::Read | Kind::Write | Kind::Zero => {
                if !self.offset.is_multiple_of(BLOCK_SIZE as u64)
                    || !self.length.is_multiple_of(BLOCK_SIZE as u64)
                    || self
                        .offset
                        .checked_add(self.length)
                        .is_none_or(|end| end > image_bytes)
                    || (self.kind != Kind::Zero
                        && (self.length == 0 || self.length > MAX_REQUEST_BYTES as u64))
                {
                    return Err(invalid("invalid inflight logical range"));
                }
                Ok(())
            }
            _ => Err(invalid("control request has a payload range")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Entry {
    pub request: Request,
    pub serial: u64,
    pub mutation: u64,
    pub boundary: u64,
    pub attachment: u64,
    pub rejected: bool,
}

impl Entry {
    fn required_publication(self) -> u64 {
        if self.rejected {
            0
        } else if matches!(self.request.kind, Kind::Read | Kind::Flush) {
            self.boundary
        } else {
            self.mutation
        }
    }
}

pub struct Carrier {
    mapping: Mapping,
    geometry: Geometry,
    identity: Identity,
    image_bytes: u64,
}

#[derive(Clone, Copy)]
pub enum AdmissionPhase {
    Prepared,
    Active,
}

impl Carrier {
    pub fn create(
        geometry: Geometry,
        identity: Identity,
        image_bytes: u64,
        prefix: u64,
    ) -> io::Result<Self> {
        let file = mapping::create(geometry.bytes())?;
        let carrier = Self {
            mapping: Mapping::open(file, geometry.bytes())?,
            geometry,
            identity,
            image_bytes,
        };
        let header = carrier.header();
        header.version.store(VERSION, Relaxed);
        header.bytes.store(PAGE as u32, Relaxed);
        for (target, bytes) in [
            (&header.store, identity.store),
            (&header.image, identity.image),
        ] {
            for (word, chunk) in target.iter().zip(bytes.as_chunks::<8>().0) {
                word.store(u64::from_le_bytes(*chunk), Relaxed);
            }
        }
        header.epoch.store(identity.epoch, Relaxed);
        header.attachment.store(identity.attachment, Relaxed);
        header.queues.store(u32::from(geometry.queues), Relaxed);
        header
            .queue_size
            .store(u32::from(geometry.queue_size), Relaxed);
        header.slot_size.store(size_of::<Slot>() as u32, Relaxed);
        header.mutation.store(prefix, Relaxed);
        header.published.store(prefix, Relaxed);
        header.magic.store(MAGIC, Release);
        Ok(carrier)
    }

    /// Validates the entire mapping before any reconciliation writes. The
    /// caller has already acquired the actual mutable segment file locks.
    pub fn attach(
        file: File,
        message: &VhostUserInflight,
        identity: Identity,
        image_bytes: u64,
    ) -> io::Result<Self> {
        let geometry = Geometry::new(message.num_queues, message.queue_size)?;
        if message.mmap_offset != 0 || message.mmap_size != geometry.bytes() as u64 {
            return Err(invalid("inflight mapping geometry differs"));
        }
        let carrier = Self {
            mapping: Mapping::open(file, geometry.bytes())?,
            geometry,
            identity,
            image_bytes,
        };
        carrier.validate_header()?;
        carrier.scan()?;
        Ok(carrier)
    }

    pub fn export(&self) -> io::Result<(VhostUserInflight, File)> {
        Ok((self.geometry.message(), self.mapping.file.try_clone()?))
    }

    pub fn identity(&self) -> Identity {
        self.identity
    }

    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    pub fn queue_initialized(&self, queue: u16) -> io::Result<bool> {
        Ok(self.queue(queue)?.version.load(Acquire) == 1)
    }

    fn header(&self) -> &Header {
        // SAFETY: Geometry locates the aligned, bounded atomic-only header.
        unsafe { self.mapping.at(self.geometry.trailer()) }
    }

    fn queue(&self, queue: u16) -> io::Result<&Queue> {
        self.geometry.check(queue, 0)?;
        // SAFETY: validated queue index and geometry bound this atomic layout.
        Ok(unsafe { self.mapping.at(usize::from(queue) * self.geometry.stride()) })
    }

    fn descriptor(&self, queue: u16, head: u16) -> io::Result<&Descriptor> {
        self.geometry.check(queue, head)?;
        let offset = usize::from(queue) * self.geometry.stride()
            + size_of::<Queue>()
            + usize::from(head) * size_of::<Descriptor>();
        // SAFETY: checked head and queue locate an aligned atomic descriptor.
        Ok(unsafe { self.mapping.at(offset) })
    }

    fn slot(&self, queue: u16, head: u16) -> io::Result<&Slot> {
        self.geometry.check(queue, head)?;
        let index = usize::from(queue) * usize::from(self.geometry.queue_size) + usize::from(head);
        // SAFETY: checked indices locate a 64-byte-aligned atomic-only slot.
        Ok(unsafe {
            self.mapping
                .at(self.geometry.trailer() + PAGE + index * size_of::<Slot>())
        })
    }

    fn healthy(&self) -> io::Result<()> {
        if self.header().failed.load(Acquire) != 0 {
            return Err(invalid(
                "FAILED attachment requires explicit recovery or a fresh reset",
            ));
        }
        Ok(())
    }

    pub fn fail(&mut self) {
        self.header().failed.store(1, Release);
    }

    /// Called under the image sequencer; no admission/retirement can interleave.
    pub fn oldest_live_mutation(&self) -> io::Result<Option<u64>> {
        self.healthy()?;
        let mut oldest = None;
        for queue in 0..self.geometry.queues {
            for head in 0..self.geometry.queue_size {
                if let Some((_, entry)) = self.read_slot(queue, head)?
                    && entry.mutation != 0
                {
                    oldest =
                        Some(oldest.map_or(entry.mutation, |old: u64| old.min(entry.mutation)));
                }
            }
        }
        Ok(oldest)
    }

    pub fn published(&self) -> u64 {
        self.header().published.load(Acquire)
    }

    /// Called only after the ordered logical index has published this prefix.
    pub fn publish(&mut self, prefix: u64) -> io::Result<()> {
        self.healthy()?;
        if prefix < self.published() || prefix > self.header().mutation.load(Acquire) {
            return Err(invalid("published prefix outside issued mutation range"));
        }
        self.header().published.store(prefix, Release);
        Ok(())
    }

    /// A never-enabled queue takes its initial cursors from guest ring state.
    /// Reconnecting queues use reconcile(), never overwrite saved cursors.
    pub fn initialize_queue(&mut self, queue: u16, available: u16, used: u16) -> io::Result<()> {
        self.healthy()?;
        let standard = self.queue(queue)?;
        if standard.version.load(Acquire) != 0 {
            return Err(invalid("inflight queue already initialized"));
        }
        standard
            .descriptors
            .store(self.geometry.queue_size, Relaxed);
        standard.used.store(used, Relaxed);
        self.header().available[usize::from(queue)].store(u32::from(available), Relaxed);
        standard.version.store(1, Release);
        Ok(())
    }

    pub fn available(&self, queue: u16) -> io::Result<u16> {
        self.geometry.check(queue, 0)?;
        self.header().available[usize::from(queue)]
            .load(Acquire)
            .try_into()
            .map_err(|_| invalid("inflight available cursor exceeds u16"))
    }

    /// Rebase a quiesced queue after frontend reset. Global identities and P
    /// remain unchanged; an outstanding descriptor forbids reinitialization.
    pub fn reset_queue(&mut self, queue: u16, available: u16, used: u16) -> io::Result<()> {
        self.healthy()?;
        let standard = self.queue(queue)?;
        if standard.version.load(Acquire) != 1 {
            return Err(invalid("cannot reset an uninitialized inflight queue"));
        }
        for head in 0..self.geometry.queue_size {
            if self.slot(queue, head)?.state.load(Acquire) != EMPTY
                || self.descriptor(queue, head)?.inflight.load(Acquire) != 0
            {
                return Err(invalid("cannot reset a queue with outstanding ownership"));
            }
        }
        standard.used.store(used, Release);
        self.header().available[usize::from(queue)].store(u32::from(available), Release);
        Ok(())
    }

    pub fn admit(&mut self, request: Request) -> io::Result<Entry> {
        self.admit_outcome(request, false)
    }

    /// Preserve an admission rejection across a crash without issuing a mutation.
    pub fn reject(&mut self, request: Request) -> io::Result<Entry> {
        self.admit_outcome(request, true)
    }

    fn admit_outcome(&mut self, request: Request, rejected: bool) -> io::Result<Entry> {
        self.admit_observed(request, rejected, |_| Ok(()))
    }

    /// Observe published admission phases before their next transition. An
    /// interrupted observer leaves the preceding phase available to recovery.
    pub fn admit_observed(
        &mut self,
        request: Request,
        rejected: bool,
        mut observe: impl FnMut(AdmissionPhase) -> io::Result<()>,
    ) -> io::Result<Entry> {
        let entry = self.prepare(request, rejected)?;
        observe(AdmissionPhase::Prepared)?;
        self.activate(entry)?;
        observe(AdmissionPhase::Active)?;
        self.finish_admission(entry);
        Ok(entry)
    }

    fn prepare(&mut self, request: Request, rejected: bool) -> io::Result<Entry> {
        self.healthy()?;
        request.validate(self.image_bytes)?;
        if self.queue(request.queue)?.version.load(Acquire) != 1
            || self.available(request.queue)? != request.available
        {
            return Err(invalid("request does not match saved admission cursor"));
        }
        let slot = self.slot(request.queue, request.head)?;
        if slot.state.load(Acquire) != EMPTY
            || self
                .descriptor(request.queue, request.head)?
                .inflight
                .load(Acquire)
                != 0
        {
            return Err(invalid("inflight head reused before retirement"));
        }
        let serial = self
            .header()
            .serial
            .load(Acquire)
            .checked_add(1)
            .ok_or_else(|| invalid("operation serial exhausted"))?;
        let boundary = self.header().mutation.load(Acquire);
        let mutation = if request.mutates() && !rejected {
            boundary
                .checked_add(1)
                .ok_or_else(|| invalid("mutation sequence exhausted"))?
        } else {
            0
        };
        let entry = Entry {
            request,
            serial,
            mutation,
            boundary,
            attachment: self.identity.attachment,
            rejected,
        };
        slot.flags
            .store(if rejected { REJECTED } else { 0 }, Relaxed);
        slot.kind.store(request.kind as u16, Relaxed);
        slot.queue.store(request.queue, Relaxed);
        slot.head.store(request.head, Relaxed);
        slot.available.store(request.available, Relaxed);
        slot.serial.store(serial, Relaxed);
        slot.mutation.store(mutation, Relaxed);
        slot.boundary.store(boundary, Relaxed);
        slot.offset.store(request.offset, Relaxed);
        slot.length.store(request.length, Relaxed);
        slot.attachment.store(entry.attachment, Relaxed);
        slot.state.store(PREPARED, Release);
        Ok(entry)
    }

    fn activate(&self, entry: Entry) -> io::Result<()> {
        let descriptor = self.descriptor(entry.request.queue, entry.request.head)?;
        descriptor.counter.store(entry.serial, Relaxed);
        descriptor.inflight.store(1, Release);
        self.slot(entry.request.queue, entry.request.head)?
            .state
            .store(ACTIVE, Release);
        Ok(())
    }

    fn finish_admission(&self, entry: Entry) {
        self.header().serial.store(entry.serial, Release);
        self.header()
            .mutation
            .store(entry.mutation.max(entry.boundary), Release);
        self.header().available[usize::from(entry.request.queue)]
            .store(u32::from(entry.request.available.wrapping_add(1)), Release);
    }

    /// `publish_used` writes guest status, used element and used.idx in order.
    /// A failure poisons the attachment before any other completion can run.
    pub fn complete(
        &mut self,
        entry: Entry,
        used: u16,
        publish_used: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.begin_completion(entry, used)?;
        if let Err(error) = publish_used() {
            self.fail();
            return Err(error);
        }
        self.finish_completion(entry, used.wrapping_add(1))
    }

    fn begin_completion(&self, entry: Entry, used: u16) -> io::Result<()> {
        self.prepare_completion(entry, used, false)
    }

    /// Only IOERR publication may retire an unpublished mutation after FAILED.
    /// The completion lock must cover fail(), this call and the guest writes.
    pub fn complete_error(
        &mut self,
        entry: Entry,
        used: u16,
        publish_used: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.prepare_completion(entry, used, true)?;
        publish_used()?;
        self.finish_completion(entry, used.wrapping_add(1))
    }

    fn prepare_completion(&self, entry: Entry, used: u16, failed: bool) -> io::Result<()> {
        if failed {
            if self.header().failed.load(Acquire) != 1 {
                return Err(invalid("IOERR retirement requires a FAILED attachment"));
            }
        } else {
            self.healthy()?;
        }
        let (state, actual) = self
            .read_slot(entry.request.queue, entry.request.head)?
            .ok_or_else(|| invalid("completion has no active request"))?;
        if state != ACTIVE
            || actual != entry
            || self.queue(entry.request.queue)?.used.load(Acquire) != used
        {
            return Err(invalid("completion identity or used cursor differs"));
        }
        if !failed && entry.required_publication() > self.published() {
            return Err(invalid(
                "completion precedes its captured publication boundary",
            ));
        }
        self.descriptor(entry.request.queue, entry.request.head)?
            .next
            .store(entry.request.head, Relaxed);
        self.queue(entry.request.queue)?
            .last_head
            .store(entry.request.head, Release);
        Ok(())
    }

    fn finish_completion(&self, entry: Entry, used: u16) -> io::Result<()> {
        self.descriptor(entry.request.queue, entry.request.head)?
            .inflight
            .store(0, Release);
        self.queue(entry.request.queue)?.used.store(used, Release);
        self.slot(entry.request.queue, entry.request.head)?
            .state
            .store(EMPTY, Release);
        Ok(())
    }
}
