//! Inspect under exclusive IO locks before choosing fresh or live recovery.
use std::{
    collections::VecDeque,
    fs::{self, File},
    io,
    path::Path,
    sync::Arc,
};

use super::{
    Config, Error, Limits, Log, Result,
    format::{self, Header},
    index::Index,
    segment::{self, Directory, Segment},
};
use crate::{BLOCK_SIZE, aligned::AlignedBuffer, direct};

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
        let directory = Directory::open(path.as_ref())?;
        let segment::Candidates {
            highest: highest_segment,
            files: candidates,
        } = super::segment::candidates(&directory)?;
        let (number, file) = candidates
            .first()
            .ok_or_else(|| io::Error::other("no valid image segment"))?;
        let first = Segment::open(*number, Arc::clone(file))?;
        let h = first.header;
        if h.preceding_sequence != 0 {
            return Err(io::Error::other("missing initial staging prefix").into());
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
        let mut log = Self {
            directory,
            config,
            limits,
            segments: Vec::new(),
            index: Index::default(),
            offset: BLOCK_SIZE as u64,
            next_batch: 1,
            highest_segment,
            published: 0,
            issued: 0,
            pending_descriptors: 0,
            cohort: None,
            durable: 0,
            encoded_bytes: 0,
            allocated_bytes: 0,
            rejected_bytes: 0,
            failed: false,
            fenced: false,
        };
        let mut rejected = None;
        for (index, (number, file)) in candidates.iter().enumerate() {
            let segment = match Segment::open(*number, Arc::clone(file)) {
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
                if !log.replay_one(&segment, length)? {
                    rejected = Some((index, log.offset));
                    break;
                }
            }
            if rejected.is_some() {
                break;
            }
        }
        log.issued = log.published;
        Ok(Recovery {
            log,
            candidates,
            rejected,
        })
    }

    fn replay_one(&mut self, segment: &Arc<Segment>, file_length: u64) -> Result<bool> {
        if file_length - self.offset < BLOCK_SIZE as u64 {
            return Ok(false);
        }
        let mut buffer = AlignedBuffer::new(BLOCK_SIZE);
        direct::read(&segment.file, &mut buffer, self.offset)?;
        let Ok(header) = Header::decode(buffer.as_slice(), self.config.image_bytes) else {
            return Ok(false);
        };
        let envelope = header.envelope();
        let length = (BLOCK_SIZE + envelope.payload_bytes) as u64;
        if envelope.segment != segment.header.number
            || envelope.batch != self.next_batch
            || self.offset + length > file_length
            || self.offset + length > segment.header.capacity
        {
            return Ok(false);
        }
        if envelope.fence {
            if envelope.last != self.published {
                return Ok(false);
            }
        } else {
            if self.published.checked_add(1) != Some(envelope.first)
                || self.offset + length + BLOCK_SIZE as u64 > segment.header.capacity
            {
                return Ok(false);
            }
            if self.index.len() + 2 * envelope.descriptors > self.limits.intervals {
                return Err(Error::Capacity);
            }
            if envelope.payload_bytes != 0 {
                let mut payload = AlignedBuffer::new(envelope.payload_bytes);
                direct::read(&segment.file, &mut payload, self.offset + BLOCK_SIZE as u64)?;
                if header.verify_payload(payload.as_slice()).is_err() {
                    return Ok(false);
                }
            }
            self.publish(&header, Arc::clone(segment), self.offset);
        }
        self.offset += length;
        self.encoded_bytes += length;
        self.next_batch = self.next_batch.checked_add(1).ok_or(Error::Exhausted)?;
        Ok(true)
    }
}

/// Read-only inspection. Its locks cover all candidate files, including the
/// rejected suffix, until validation and repair have finished.
pub struct Recovery {
    log: Log,
    candidates: Vec<(u64, Arc<File>)>,
    rejected: Option<(usize, u64)>,
}

impl Recovery {
    pub fn config(&self) -> Config {
        self.log.config
    }
    pub fn status(&self) -> super::Status {
        self.log.status()
    }

    fn require_prefix(&self, required: u64) -> Result<()> {
        if self.log.published < required {
            return Err(Error::Prefix {
                recovered: self.log.published,
                required,
            });
        }
        Ok(())
    }

    pub fn fresh(mut self, required: u64) -> Result<Log> {
        self.require_prefix(required)?;
        self.repair()?;
        // The fresh segment ticket is also a unique, increasing epoch ticket.
        let epoch = self
            .log
            .highest_segment
            .checked_add(1)
            .ok_or(Error::Exhausted)?;
        self.log.rotate(epoch)?;
        self.log.flush()?;
        Ok(self.log)
    }

    /// `mutations` names every inflight mutation, including overwritten writes.
    /// Shared-state reconciliation and guest descriptor validation precede this
    /// call. Missing tail mutations must all still have an owned request.
    pub fn live(
        mut self,
        required: u64,
        epoch: u64,
        highest_issued: u64,
        mut mutations: Vec<Mutation>,
    ) -> Result<LiveRecovery> {
        self.require_prefix(required)?;
        let prefix = self.log.published;
        if epoch != self.log.current().header.epoch
            || prefix > highest_issued
            || mutations.len() > 1024
        {
            return Err(
                io::Error::other("live epoch, issued prefix or replay bound differs").into(),
            );
        }
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
        self.repair()?;
        // A retained fence may fill its segment. All retained files have synced
        // before creating a successor, but only finish() establishes recovered E.
        if self.log.offset + BLOCK_SIZE as u64 > self.log.config.segment_bytes {
            self.log.rotate(epoch)?;
        }
        let remaining = mutations
            .into_iter()
            .filter(|m| m.sequence > prefix)
            .collect();
        Ok(LiveRecovery {
            log: self.log,
            remaining,
        })
    }

    fn verify_identities(&self, mutations: &[Mutation]) -> Result<()> {
        let expected = mutations.partition_point(|m| m.sequence <= self.log.published);
        if expected == 0 {
            return Ok(());
        }
        let mut found = 0;
        let mut buffer = AlignedBuffer::new(BLOCK_SIZE);
        for (index, segment) in self.log.segments.iter().enumerate() {
            let limit = if index + 1 == self.log.segments.len() {
                self.log.offset
            } else {
                segment.file.metadata()?.len()
            };
            let mut offset = BLOCK_SIZE as u64;
            while offset < limit {
                direct::read(&segment.file, &mut buffer, offset)?;
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

    fn repair(&mut self) -> Result<()> {
        if let Some((index, offset)) = self.rejected.take() {
            // Retain every rejected byte before the first destructive change.
            for (relative, (number, file)) in self.candidates[index..].iter().enumerate() {
                let begin = if relative == 0 { offset } else { 0 };
                self.log
                    .directory
                    .archive(&super::segment::name(*number), file, begin)?;
                self.log.rejected_bytes += file.metadata()?.len() - begin;
            }
            for (relative, (number, file)) in self.candidates[index..].iter().enumerate() {
                if relative == 0 && offset != 0 {
                    file.set_len(offset)?;
                    file.sync_all()?;
                } else {
                    fs::remove_file(self.log.directory.path.join(segment::name(*number)))?;
                }
            }
            self.log.directory.sync()?;
        }
        for segment in &self.log.segments {
            direct::sync_data(&segment.file)?;
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
pub struct LiveRecovery {
    log: Log,
    remaining: VecDeque<Mutation>,
}

impl LiveRecovery {
    pub fn next(&self) -> Option<Mutation> {
        self.remaining.front().copied()
    }
    pub fn published(&self) -> u64 {
        self.log.published
    }

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
        self.log.append(builder)?;
        self.remaining.pop_front();
        Ok(mutation)
    }

    pub fn finish(mut self) -> Result<Log> {
        if !self.remaining.is_empty() {
            return Err(io::Error::other("replay has unresolved mutations").into());
        }
        self.log.flush()?;
        Ok(self.log)
    }
}
