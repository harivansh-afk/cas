//! Inspect under exclusive IO locks before choosing fresh or live recovery.
use std::{fs::File, io, path::Path, sync::Arc};

use super::{
    Config, Error, Limits, Log, Result,
    format::{self, Header},
    index::Index,
    segment::{self, Directory, Segment},
};
use crate::budget::BudgetAllocator;
use crate::{BLOCK_SIZE, aligned::AlignedBuffer, direct};
use allocator_api2::vec::Vec as BudgetVec;

impl Log {
    pub fn open(path: impl AsRef<Path>, limits: Limits) -> Result<Self> {
        Self::open_with_expected_prefix(path, limits, 0)
    }

    /// An external crash oracle can require a known prefix. Recovery never
    /// repairs a file before this condition has passed. C3 supplies live P from
    /// shared inflight state; this cold API does not pretend to recover that P.
    pub fn open_with_expected_prefix(
        path: impl AsRef<Path>,
        limits: Limits,
        required: u64,
    ) -> Result<Self> {
        Self::inspect(path, limits)?.fresh(required)
    }

    pub fn inspect(path: impl AsRef<Path>, limits: Limits) -> Result<Recovery> {
        Self::inspect_with_metadata(path, limits, super::default_metadata())
    }

    /// Reserve interval nodes before read-only inspection; no repair occurs here.
    pub fn inspect_with_metadata(
        path: impl AsRef<Path>,
        limits: Limits,
        metadata: Arc<crate::budget::Budget>,
    ) -> Result<Recovery> {
        Self::inspect_in(path.as_ref(), limits, metadata, 0, None)
    }

    pub(super) fn inspect_in(
        path: &Path,
        limits: Limits,
        metadata: Arc<crate::budget::Budget>,
        base: u64,
        tickets: Option<Arc<crate::segments::Tickets>>,
    ) -> Result<Recovery> {
        let index = Index::new(limits.intervals, Arc::clone(&metadata))?;
        let directory = Directory::open(path)?;
        let segment::Candidates {
            highest: highest_segment,
            files: candidates,
        } = super::segment::candidates(&directory, Arc::clone(&metadata))?;
        let (number, file) = candidates
            .first()
            .ok_or_else(|| io::Error::other("no valid image segment"))?;
        let first = Segment::open(*number, Arc::clone(file), Arc::clone(&metadata))?;
        let h = first.header;
        if h.preceding_sequence > base {
            return Err(io::Error::other("missing staging prefix above manifest D").into());
        }
        let config = Config {
            store: h.store,
            image: h.image,
            image_bytes: h.image_bytes,
            segment_bytes: h.capacity,
        };
        if config.segment_bytes < (format::MAX_BATCH_BYTES + 2 * BLOCK_SIZE) as u64 {
            return Err(Error::Capacity);
        }
        let mut log = Self::empty(
            directory,
            config,
            limits,
            index,
            Arc::clone(&metadata),
            highest_segment,
            tickets,
            None,
            h.preceding_sequence,
        );
        log.segments
            .try_reserve_exact(candidates.len())
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "staging segment table exhausted",
                )
            })?;
        let mut rejected = None;
        for (index, (number, file)) in candidates.iter().enumerate() {
            let opened = if index == 0 {
                Ok(Arc::clone(&first))
            } else {
                Segment::open(*number, Arc::clone(file), Arc::clone(&metadata))
            };
            let segment = match opened {
                Ok(segment) => segment,
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                    rejected = Some((index, 0));
                    break;
                }
                Err(error) => return Err(error.into()),
            };
            if !config.matches(segment.header)
                || segment.header.preceding_sequence != log.published
                || log
                    .segments
                    .last()
                    .is_some_and(|previous| segment.header.epoch < previous.header.epoch)
            {
                return Err(io::Error::other("inconsistent staging segment chain").into());
            }
            log.offset = BLOCK_SIZE as u64;
            log.next_batch = 1;
            log.encoded_bytes += BLOCK_SIZE as u64;
            log.allocated_bytes += segment.allocated_bytes()?;
            log.segments.push(Arc::clone(&segment));
            let length = segment.file.metadata()?.len();
            while log.offset < length {
                if !log.replay_one(&segment, length, base)? {
                    rejected = Some((index, log.offset));
                    break;
                }
            }
            segment
                .end
                .store(log.offset, std::sync::atomic::Ordering::Relaxed);
            if rejected.is_some() {
                break;
            }
        }
        if log.published < base {
            return Err(Error::Prefix {
                recovered: log.published,
                required: base,
            });
        }
        log.issued = log.published;
        Ok(Recovery {
            log,
            candidates,
            rejected,
        })
    }

    fn replay_one(&mut self, segment: &Arc<Segment>, file_length: u64, base: u64) -> Result<bool> {
        if file_length - self.offset < BLOCK_SIZE as u64 {
            return Ok(false);
        }
        let mut buffer = AlignedBuffer::try_new_in(
            BLOCK_SIZE,
            BudgetAllocator::new(Arc::clone(&self.metadata)),
        )?;
        direct::read_bytes(&segment.file, buffer.as_mut_slice(), self.offset)?;
        let Ok(header) = Header::decode(buffer.as_slice(), self.config.image_bytes) else {
            return Ok(false);
        };
        let envelope = header.envelope();
        let length = (BLOCK_SIZE + envelope.payload_bytes) as u64;
        if !header.follows(
            segment.header,
            self.next_batch,
            self.published,
            self.offset,
            file_length,
        ) {
            return Ok(false);
        }
        if !envelope.fence {
            if envelope.first <= base && base < envelope.last {
                return Err(io::Error::other("manifest D splits a staging batch").into());
            }
            if envelope.last <= base {
                // Headers remain for framing and live identities; the durable
                // manifest supplies data whose WAL payload may be punched.
                self.published = envelope.last;
            } else {
                if self.index.len() + 2 * envelope.descriptors > self.limits.intervals {
                    return Err(Error::Capacity);
                }
                if envelope.payload_bytes != 0 {
                    let mut payload = AlignedBuffer::try_new_in(
                        envelope.payload_bytes,
                        BudgetAllocator::new(Arc::clone(&self.metadata)),
                    )?;
                    direct::read_bytes(
                        &segment.file,
                        payload.as_mut_slice(),
                        self.offset + BLOCK_SIZE as u64,
                    )?;
                    if header.verify_payload(payload.as_slice()).is_err() {
                        return Ok(false);
                    }
                }
                self.publish(&header, Arc::clone(segment), self.offset);
            }
        }
        self.offset += length;
        self.encoded_bytes += length;
        self.next_batch = self.next_batch.checked_add(1).ok_or(Error::Exhausted)?;
        if self.published == base {
            self.compaction_cursor = Some(super::compaction::ScanPosition {
                segment: segment.header.number,
                offset: self.offset,
                batch: self.next_batch,
                sequence: base,
            });
        }
        Ok(true)
    }
}

/// Read-only inspection. Its locks cover all candidate files, including the
/// rejected suffix, until validation and repair have finished.
pub struct Recovery {
    pub(super) log: Log,
    candidates: BudgetVec<(u64, Arc<File>), BudgetAllocator>,
    rejected: Option<(usize, u64)>,
}

impl Recovery {
    pub fn candidate_bytes(&self) -> usize {
        self.candidates.capacity() * size_of::<(u64, Arc<File>)>()
    }

    pub fn config(&self) -> Config {
        self.log.config
    }
    pub fn status(&self) -> super::Status {
        self.log.status()
    }

    pub(super) fn require_prefix(&self, required: u64) -> Result<()> {
        if self.log.published < required {
            return Err(Error::Prefix {
                recovered: self.log.published,
                required,
            });
        }
        Ok(())
    }

    pub fn fresh(self, required: u64) -> Result<Log> {
        self.fresh_with(required, crate::space::Recovery::default())
    }

    pub fn fresh_with(mut self, required: u64, repair: crate::space::Recovery<'_>) -> Result<Log> {
        self.require_prefix(required)?;
        self.repair(repair)?;
        repair.output(self.log.config.segment_bytes, || {
            self.log.rotate(None)?;
            self.log.flush()
        })?;
        Ok(self.log)
    }

    /// `mutations` names every inflight mutation, including overwritten writes.
    /// Shared-state reconciliation and guest descriptor validation precede this
    /// call. Missing tail mutations must all still have an owned request.
    pub fn live(
        self,
        required: u64,
        epoch: u64,
        highest_issued: u64,
        mutations: Vec<Mutation>,
    ) -> Result<LiveRecovery> {
        self.prepare_live(required, epoch, highest_issued, mutations)?
            .start(crate::space::Recovery::default())
    }

    pub fn prepare_live(
        self,
        required: u64,
        epoch: u64,
        highest_issued: u64,
        mutations: impl IntoIterator<Item = Mutation>,
    ) -> Result<LivePlan> {
        self.require_prefix(required)?;
        let prefix = self.log.published;
        if epoch != self.log.current().header.epoch || prefix > highest_issued {
            return Err(
                io::Error::other("live epoch, issued prefix or replay bound differs").into(),
            );
        }
        let mut retained = BudgetVec::new_in(BudgetAllocator::new(Arc::clone(&self.log.metadata)));
        for mutation in mutations {
            if retained.len() == 1024 {
                return Err(io::Error::other("live mutation count exceeds replay bound").into());
            }
            retained.try_reserve(1).map_err(|_| {
                io::Error::new(io::ErrorKind::OutOfMemory, "live mutation table exhausted")
            })?;
            retained.push(mutation);
        }
        let mut mutations = retained;
        mutations.sort_unstable_by_key(|mutation| mutation.sequence);
        let mut next = prefix;
        for (i, mutation) in mutations.iter().enumerate() {
            if mutation.sequence == 0
                || mutation.sequence > highest_issued
                || mutation.id.attachment == 0
                || mutation.id.serial == 0
                || mutation.id.queue >= 4
                || mutation.id.head >= 256
                || mutation.length == 0
                || !mutation.offset.is_multiple_of(BLOCK_SIZE as u64)
                || !mutation.length.is_multiple_of(BLOCK_SIZE as u64)
                || mutation
                    .offset
                    .checked_add(mutation.length)
                    .is_none_or(|end| end > self.log.config.image_bytes)
                || (mutation.kind == format::Kind::Write
                    && mutation.length > crate::MAX_REQUEST_BYTES as u64)
                || (i != 0
                    && (mutations[i - 1].sequence == mutation.sequence
                        || mutations[i - 1].id.serial >= mutation.id.serial))
            {
                return Err(io::Error::other("invalid or duplicate live mutation identity").into());
            }
            if mutation.sequence > prefix {
                next = next.checked_add(1).ok_or(Error::Exhausted)?;
                if mutation.sequence != next {
                    return Err(
                        io::Error::other("missing tail mutation has no inflight owner").into(),
                    );
                }
            }
        }
        if next != highest_issued {
            return Err(io::Error::other("issued tail has no inflight owner").into());
        }
        self.verify_identities(&mutations)?;
        mutations.retain(|mutation| mutation.sequence > prefix);
        Ok(LivePlan {
            recovery: self,
            remaining: mutations,
            epoch,
        })
    }

    fn verify_identities(&self, mutations: &[Mutation]) -> Result<()> {
        let expected = mutations.partition_point(|m| m.sequence <= self.log.published);
        if expected == 0 {
            return Ok(());
        }
        let mut found = 0;
        let mut buffer = AlignedBuffer::try_new_in(
            BLOCK_SIZE,
            BudgetAllocator::new(Arc::clone(&self.log.metadata)),
        )?;
        for (index, segment) in self.log.segments.iter().enumerate() {
            let limit = if index + 1 == self.log.segments.len() {
                self.log.offset
            } else {
                segment.file.metadata()?.len()
            };
            let mut offset = BLOCK_SIZE as u64;
            while offset < limit {
                direct::read_bytes(&segment.file, buffer.as_mut_slice(), offset)?;
                let header = Header::decode(buffer.as_slice(), self.log.config.image_bytes)?;
                for descriptor in header.descriptors() {
                    if let Ok(index) =
                        mutations.binary_search_by_key(&descriptor.sequence, |m| m.sequence)
                    {
                        if mutations[index] != Mutation::from(descriptor) {
                            return Err(io::Error::other(
                                "retained mutation identity or range differs",
                            )
                            .into());
                        }
                        found += 1;
                    }
                }
                offset += (BLOCK_SIZE + header.envelope().payload_bytes) as u64;
            }
        }
        if found != expected {
            return Err(io::Error::other("retained inflight mutation was not found").into());
        }
        Ok(())
    }

    pub fn validate_recovery(&self, repair: crate::space::Recovery<'_>) -> Result<()> {
        repair.validate_tickets(self.log.tickets.as_ref())?;
        for (_, file) in &self.candidates {
            repair.validate_file(file)?;
        }
        if let Some((index, offset)) = self.rejected {
            for (relative, (_, file)) in self.candidates[index..].iter().enumerate() {
                let begin = if relative == 0 { offset } else { 0 };
                repair.validate_output(Directory::archive_bytes(file, begin)?)?;
            }
        }
        repair.validate_output(self.log.config.segment_bytes)?;
        Ok(())
    }

    fn repair(&mut self, repair: crate::space::Recovery<'_>) -> Result<()> {
        self.validate_recovery(repair)?;
        if let Some((index, offset)) = self.rejected.take() {
            // Retain every rejected byte before the first destructive change.
            for (relative, (number, file)) in self.candidates[index..].iter().enumerate() {
                let begin = if relative == 0 { offset } else { 0 };
                repair.archive(
                    &self.log.directory,
                    &super::segment::name(*number),
                    file,
                    begin,
                )?;
                self.log.rejected_bytes += file.metadata()?.len() - begin;
            }
            for (relative, (number, file)) in self.candidates[index..].iter().enumerate() {
                repair.output(0, || {
                    if relative == 0 && offset != 0 {
                        direct::truncate(file, offset)?;
                        direct::sync_all(file)?;
                    } else {
                        self.log.directory.remove(&segment::name(*number))?;
                    }
                    self.log.directory.sync()
                })?;
            }
        }
        for segment in &self.log.segments {
            repair.validate_file(&segment.file)?;
            repair.output(0, || direct::sync_data(&segment.file))?;
        }
        Ok(())
    }
}

/// The identity portion of a WAL descriptor; payload locations and CRCs are
/// independently checked by the WAL decoder and original payload verifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mutation {
    pub id: format::RequestId,
    pub sequence: u64,
    pub offset: u64,
    pub length: u64,
    pub kind: format::Kind,
}

impl From<format::Descriptor> for Mutation {
    fn from(value: format::Descriptor) -> Self {
        Self {
            id: value.id,
            sequence: value.sequence,
            offset: value.offset,
            length: value.length,
            kind: value.kind,
        }
    }
}

/// A serving Log is unavailable until replay and its recovery fence finish.
/// Dropping this handle is safe; another replacement reinspects the same WAL.
pub struct LivePlan {
    pub(super) recovery: Recovery,
    remaining: BudgetVec<Mutation, BudgetAllocator>,
    epoch: u64,
}

impl LivePlan {
    pub fn validate_recovery(&self, repair: crate::space::Recovery<'_>) -> Result<()> {
        self.recovery.validate_recovery(repair)
    }

    pub fn start(mut self, repair: crate::space::Recovery<'_>) -> Result<LiveRecovery> {
        self.recovery.repair(repair)?;
        let mut log = self.recovery.log;
        // A retained fence may fill its segment. Only finish establishes E.
        if log.offset + BLOCK_SIZE as u64 > log.config.segment_bytes {
            repair.output(log.config.segment_bytes, || log.rotate(Some(self.epoch)))?;
        }
        Ok(LiveRecovery {
            log,
            remaining: self.remaining,
            next: 0,
            physical: repair.physical(),
        })
    }
}

pub struct LiveRecovery {
    log: Log,
    remaining: BudgetVec<Mutation, BudgetAllocator>,
    next: usize,
    physical: Option<Arc<crate::space::Governor>>,
}

/// Replay borrows the physical owner captured at start, or runs ungoverned.
fn repair(physical: &Option<Arc<crate::space::Governor>>) -> crate::space::Recovery<'_> {
    physical.as_ref().map_or_else(
        crate::space::Recovery::default,
        crate::space::Recovery::governed,
    )
}

impl LiveRecovery {
    pub fn next(&self) -> Option<Mutation> {
        self.remaining.get(self.next).copied()
    }
    pub fn published(&self) -> u64 {
        self.log.published
    }

    /// The caller reserves the append buffer before gathering guest payload.
    pub fn replay_next(
        &mut self,
        gather: impl FnOnce(&mut [u8]) -> io::Result<()>,
    ) -> Result<Mutation> {
        let mutation = self
            .next()
            .ok_or_else(|| io::Error::other("no missing mutation to replay"))?;
        let payload = if mutation.kind == format::Kind::Write {
            mutation.length as usize
        } else {
            0
        };
        let mut builder = format::Builder::new(self.log.config.image_bytes, payload)?;
        match mutation.kind {
            format::Kind::Write => builder.write(mutation.id, mutation.offset, payload, gather)?,
            format::Kind::Zero => builder.zero(mutation.id, mutation.offset, mutation.length)?,
        }
        if self.log.published.checked_add(1) != Some(mutation.sequence) {
            return Err(io::Error::other("replay is not the next original mutation").into());
        }
        let repair = repair(&self.physical);
        repair.output(self.log.config.segment_bytes, || self.log.append(builder))?;
        self.next += 1;
        Ok(mutation)
    }

    pub fn finish(mut self) -> Result<Log> {
        if self.next != self.remaining.len() {
            return Err(io::Error::other("replay has unresolved mutations").into());
        }
        let repair = repair(&self.physical);
        repair.output(self.log.config.segment_bytes, || self.log.flush())?;
        Ok(self.log)
    }
}
