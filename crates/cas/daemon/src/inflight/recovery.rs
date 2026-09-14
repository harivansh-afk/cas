use allocator_api2::vec::Vec as BudgetVec;
use cas_core::budget::BudgetAllocator;
use std::io;
use std::sync::atomic::Ordering::{Acquire, Release};

use super::{
    ACTIVE, Carrier, DISCOVERED, EMPTY, Entry, Kind, MAGIC, PAGE, PREPARED, REJECTED, Request,
    Slot, VERSION, invalid,
};

pub struct Replay {
    /// Original admission order across all queues, including protocol requests.
    pub entries: BudgetVec<Entry, BudgetAllocator>,
    pub discovered: BudgetVec<(u64, Request), BudgetAllocator>,
    pub highest_discovery: u64,
    pub highest_serial: u64,
    pub highest_mutation: u64,
    pub published: u64,
}

pub(super) struct Saved {
    state: u32,
    entry: Entry,
    inflight: bool,
}

impl Carrier {
    pub(super) fn validate_header(&self) -> io::Result<()> {
        let header = self.header();
        if header.magic.load(Acquire) != MAGIC
            || header.version.load(Acquire) != VERSION
            || header.bytes.load(Acquire) != PAGE as u32
            || header.queues.load(Acquire) != u32::from(self.geometry.queues)
            || header.queue_size.load(Acquire) != u32::from(self.geometry.queue_size)
            || header.slot_size.load(Acquire) != size_of::<Slot>() as u32
            || header.epoch.load(Acquire) != self.identity.epoch
            || header.attachment.load(Acquire) != self.identity.attachment
            || header.reserved.iter().any(|byte| byte.load(Acquire) != 0)
        {
            return Err(invalid(
                "inflight header identity, geometry or version differs",
            ));
        }
        for (source, bytes) in [
            (&header.store, self.identity.store),
            (&header.image, self.identity.image),
        ] {
            for (word, chunk) in source.iter().zip(bytes.as_chunks::<8>().0) {
                if word.load(Acquire) != u64::from_le_bytes(*chunk) {
                    return Err(invalid("inflight store or image identity differs"));
                }
            }
        }
        if self.published() > header.mutation.load(Acquire)
            || header.available[usize::from(self.geometry.queues)..]
                .iter()
                .any(|cursor| cursor.load(Acquire) != 0)
        {
            return Err(invalid("invalid inflight counters"));
        }
        self.healthy()
    }

    pub(super) fn read_slot(&self, queue: u16, head: u16) -> io::Result<Option<(u32, Entry)>> {
        let slot = self.slot(queue, head)?;
        let state = slot.state.load(Acquire);
        let flags = slot.flags.load(Acquire);
        if flags & !REJECTED != 0 || slot.tail.iter().any(|byte| byte.load(Acquire) != 0) {
            return Err(invalid("nonzero reserved inflight slot bytes"));
        }
        if state == EMPTY {
            return Ok(None);
        }
        if state != PREPARED && state != ACTIVE && state != DISCOVERED {
            return Err(invalid("unknown inflight slot state"));
        }
        let request = Request {
            kind: Kind::try_from(slot.kind.load(Acquire))?,
            queue: slot.queue.load(Acquire),
            head: slot.head.load(Acquire),
            available: slot.available.load(Acquire),
            offset: slot.offset.load(Acquire),
            length: slot.length.load(Acquire),
        };
        request.validate(self.image_bytes)?;
        let discovery = slot.discovery.load(Acquire);
        if state == DISCOVERED {
            if request.queue != queue
                || request.head != head
                || slot.attachment.load(Acquire) != self.identity.attachment
                || discovery == 0
            {
                return Err(invalid("discovered descriptor identity differs"));
            }
            // Admission may have been interrupted while preparing its fields.
            // DISCOVERED owns only this immutable header, never those counters.
            return Ok(Some((
                state,
                Entry {
                    request,
                    serial: 0,
                    mutation: 0,
                    boundary: 0,
                    attachment: self.identity.attachment,
                    rejected: false,
                    discovery,
                },
            )));
        }
        let entry = Entry {
            request,
            serial: slot.serial.load(Acquire),
            mutation: slot.mutation.load(Acquire),
            boundary: slot.boundary.load(Acquire),
            attachment: slot.attachment.load(Acquire),
            rejected: flags & REJECTED != 0,
            discovery,
        };
        let expected_mutation = if request.mutates() && !entry.rejected {
            entry.boundary.checked_add(1)
        } else {
            Some(0)
        };
        if request.queue != queue
            || request.head != head
            || entry.attachment != self.identity.attachment
            || entry.serial == 0
            || discovery == 0
            || Some(entry.mutation) != expected_mutation
        {
            return Err(invalid("inflight slot identity or mutation differs"));
        }
        Ok(Some((state, entry)))
    }

    pub(super) fn scan(&self) -> io::Result<BudgetVec<Saved, BudgetAllocator>> {
        let serial = self.header().serial.load(Acquire);
        let mutation = self.header().mutation.load(Acquire);
        let capacity = usize::from(self.geometry.queues) * usize::from(self.geometry.queue_size);
        let mut saved = crate::local::reserved_vec(capacity, &self.metadata)?;
        for queue in 0..self.geometry.queues {
            let standard = self.queue(queue)?;
            let version = standard.version.load(Acquire);
            let descriptors = standard.descriptors.load(Acquire);
            if version > 1
                || standard.features.load(Acquire) != 0
                || (version == 1 && descriptors != self.geometry.queue_size)
                || (version == 0 && descriptors != 0 && descriptors != self.geometry.queue_size)
                || standard.last_head.load(Acquire) >= self.geometry.queue_size
            {
                return Err(invalid("invalid standard inflight queue header"));
            }
            self.available(queue)?;
            for head in 0..self.geometry.queue_size {
                let descriptor = self.descriptor(queue, head)?;
                let inflight = descriptor.inflight.load(Acquire);
                if inflight > 1
                    || descriptor
                        .reserved
                        .iter()
                        .any(|byte| byte.load(Acquire) != 0)
                {
                    return Err(invalid("invalid standard inflight descriptor"));
                }
                match self.read_slot(queue, head)? {
                    None if inflight == 0 => (),
                    None => return Err(invalid("standard inflight head has no private identity")),
                    Some((state, entry)) => {
                        if version != 1
                            || entry.discovery
                                > self.header().discovery.load(Acquire).saturating_add(1)
                            || (state == DISCOVERED && inflight != 0)
                            || entry.serial > serial.saturating_add(1)
                            || entry.mutation > mutation.saturating_add(1)
                            || entry.boundary > mutation
                            || (state == ACTIVE
                                && inflight == 0
                                && entry.required_publication() > self.published())
                            || ((state == ACTIVE || inflight == 1)
                                && descriptor.counter.load(Acquire) != entry.serial)
                        {
                            return Err(invalid("inflight slot counters or queue state differ"));
                        }
                        saved.push(Saved {
                            state,
                            entry,
                            inflight: inflight == 1,
                        });
                    }
                }
            }
        }
        saved.sort_unstable_by_key(|saved| saved.entry.serial);
        if saved
            .windows(2)
            .any(|pair| pair[0].entry.serial != 0 && pair[0].entry.serial == pair[1].entry.serial)
        {
            return Err(invalid("duplicate inflight operation serial"));
        }
        let mut discoveries = crate::local::reserved_vec(saved.len(), &self.metadata)?;
        discoveries.extend(
            saved
                .iter()
                .filter_map(|saved| (saved.entry.discovery != 0).then_some(saved.entry.discovery)),
        );
        discoveries.sort_unstable();
        if discoveries.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(invalid("duplicate discovery identity"));
        }
        let mut mutations = crate::local::reserved_vec(saved.len(), &self.metadata)?;
        mutations.extend(
            saved
                .iter()
                .filter_map(|saved| (saved.entry.mutation != 0).then_some(saved.entry.mutation)),
        );
        mutations.sort_unstable();
        if mutations.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(invalid("duplicate inflight mutation sequence"));
        }
        Ok(saved)
    }

    /// Reconcile with actual guest used.idx values after acquiring exclusive
    /// writer ownership. None identifies a queue that has never been enabled.
    /// Validation finishes before the first shared-memory repair. Repair itself
    /// may be interrupted: every step preserves an accepted recovery state.
    pub fn reconcile(&mut self, guest_used: &[Option<u16>]) -> io::Result<Replay> {
        self.validate_header()?;
        let saved = self.scan()?;
        if guest_used.len() != usize::from(self.geometry.queues) {
            return Err(invalid("missing guest queue cursors"));
        }
        let mut completed = crate::local::reserved_vec(guest_used.len(), &self.metadata)?;
        let mut entries = crate::local::reserved_vec(saved.len(), &self.metadata)?;
        let mut discovered = crate::local::reserved_vec(saved.len(), &self.metadata)?;
        for (queue, guest) in guest_used.iter().enumerate() {
            let standard = self.queue(queue as u16)?;
            if standard.version.load(Acquire) == 0 && guest.is_none() {
                continue;
            }
            let used =
                guest.ok_or_else(|| invalid("initialized queue has no guest used cursor"))?;
            if standard.version.load(Acquire) != 1 {
                return Err(invalid("guest queue has no initialized inflight region"));
            }
            match used.wrapping_sub(standard.used.load(Acquire)) {
                0 => (),
                1 => {
                    let head = standard.last_head.load(Acquire);
                    let interrupted = saved
                        .iter()
                        .find(|saved| {
                            saved.entry.request.queue == queue as u16
                                && saved.entry.request.head == head
                        })
                        .ok_or_else(|| invalid("used publication has no saved head"))?;
                    if interrupted.state != ACTIVE
                        || interrupted.entry.required_publication() > self.published()
                    {
                        return Err(invalid(
                            "used publication precedes active request or published mutation",
                        ));
                    }
                    completed.push(interrupted.entry);
                }
                _ => {
                    return Err(invalid(
                        "guest used cursor exceeds one completion transaction",
                    ));
                }
            }
        }

        let header = self.header();
        let issued_serial = header.serial.load(Acquire);
        let newest_discovery = saved
            .iter()
            .filter(|saved| saved.entry.discovery != 0)
            .max_by_key(|saved| saved.entry.discovery)
            .filter(|saved| saved.entry.discovery >= header.discovery.load(Acquire));
        let highest_discovery = newest_discovery.map_or(header.discovery.load(Acquire), |saved| {
            saved.entry.discovery
        });
        if let Some(newest) = newest_discovery {
            let request = newest.entry.request;
            let cursor = self.available(request.queue)?;
            if cursor != request.available && cursor != request.available.wrapping_add(1) {
                return Err(invalid("newest discovery cannot repair available cursor"));
            }
        }
        let highest_serial = saved
            .last()
            .map_or(issued_serial, |saved| issued_serial.max(saved.entry.serial));
        let highest_mutation = saved
            .iter()
            .fold(header.mutation.load(Acquire), |high, saved| {
                high.max(saved.entry.mutation)
            });

        // Repair a guest-visible completion in the same order as normal
        // retirement. ACTIVE with inflight=0 also means used was published.
        for entry in completed.iter().copied() {
            self.finish_completion(entry, guest_used[usize::from(entry.request.queue)].unwrap())?;
        }
        for record in &saved {
            if completed.contains(&record.entry) {
                continue;
            }
            if record.state == DISCOVERED {
                discovered.push((record.entry.discovery, record.entry.request));
            } else if record.state == ACTIVE && !record.inflight {
                self.slot(record.entry.request.queue, record.entry.request.head)?
                    .state
                    .store(EMPTY, Release);
            } else {
                self.activate(record.entry)?;
                entries.push(record.entry);
            }
        }
        // Discovery owns the available cursor; storage admission owns serials.
        if let Some(newest) = newest_discovery {
            self.finish_discovery(newest.entry.request, newest.entry.discovery);
        }
        discovered.sort_unstable_by_key(|(order, _)| *order);
        header.serial.store(highest_serial, Release);
        header.mutation.store(highest_mutation, Release);
        Ok(Replay {
            entries,
            discovered,
            highest_discovery,
            highest_serial,
            highest_mutation,
            published: self.published(),
        })
    }
}
