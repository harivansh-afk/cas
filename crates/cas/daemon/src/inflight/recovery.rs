use std::io;
use std::sync::atomic::Ordering::{Acquire, Release};

use super::{ACTIVE, Carrier, EMPTY, Entry, Kind, MAGIC, PAGE, PREPARED, Request, Slot, invalid};

pub struct Replay {
    /// Original admission order across all queues, including protocol requests.
    pub entries: Vec<Entry>,
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
            || header.version.load(Acquire) != 1
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
        if slot.reserved.load(Acquire) != 0 || slot.tail.iter().any(|byte| byte.load(Acquire) != 0)
        {
            return Err(invalid("nonzero reserved inflight slot bytes"));
        }
        if state == EMPTY {
            return Ok(None);
        }
        if state != PREPARED && state != ACTIVE {
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
        let entry = Entry {
            request,
            serial: slot.serial.load(Acquire),
            mutation: slot.mutation.load(Acquire),
            boundary: slot.boundary.load(Acquire),
            attachment: slot.attachment.load(Acquire),
        };
        let expected_mutation = if request.mutates() {
            entry.boundary.checked_add(1)
        } else {
            Some(0)
        };
        if request.queue != queue
            || request.head != head
            || entry.attachment != self.identity.attachment
            || entry.serial == 0
            || Some(entry.mutation) != expected_mutation
        {
            return Err(invalid("inflight slot identity or mutation differs"));
        }
        Ok(Some((state, entry)))
    }

    pub(super) fn scan(&self) -> io::Result<Vec<Saved>> {
        let serial = self.header().serial.load(Acquire);
        let mutation = self.header().mutation.load(Acquire);
        let mut saved = Vec::new();
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
                            || entry.serial > serial.saturating_add(1)
                            || entry.mutation > mutation.saturating_add(1)
                            || entry.boundary > mutation
                            || (state == ACTIVE
                                && inflight == 0
                                && entry.mutation > self.published())
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
            .any(|pair| pair[0].entry.serial == pair[1].entry.serial)
        {
            return Err(invalid("duplicate inflight operation serial"));
        }
        let mut mutations: Vec<_> = saved
            .iter()
            .filter_map(|saved| (saved.entry.mutation != 0).then_some(saved.entry.mutation))
            .collect();
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
        let mut completed = Vec::new();
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
                    if interrupted.state != ACTIVE || interrupted.entry.mutation > self.published()
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
        let newest = saved
            .last()
            .filter(|saved| saved.entry.serial >= issued_serial);
        if let Some(newest) = newest {
            let request = newest.entry.request;
            let cursor = self.available(request.queue)?;
            if cursor != request.available && cursor != request.available.wrapping_add(1) {
                return Err(invalid(
                    "newest admission cannot repair saved available cursor",
                ));
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
        let mut entries = Vec::new();
        for record in &saved {
            if completed.contains(&record.entry) {
                continue;
            }
            if record.state == ACTIVE && !record.inflight {
                self.slot(record.entry.request.queue, record.entry.request.head)?
                    .state
                    .store(EMPTY, Release);
            } else {
                self.activate(record.entry)?;
                entries.push(record.entry);
            }
        }
        // Only the newest admission may have lagging counters/cursor. An old
        // inflight request can share today's u16 cursor after ring wrap.
        if let Some(newest) = newest {
            let request = newest.entry.request;
            header.available[usize::from(request.queue)]
                .store(u32::from(request.available.wrapping_add(1)), Release);
        }
        header.serial.store(highest_serial, Release);
        header.mutation.store(highest_mutation, Release);
        Ok(Replay {
            entries,
            highest_serial,
            highest_mutation,
            published: self.published(),
        })
    }
}
