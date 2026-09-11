//! Cold replay validates a contiguous prefix before retaining and repairing its tail.
use std::{fs, io, path::Path, sync::Arc};

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
        let directory = Directory::open(path.as_ref())?;
        let segment::Candidates {
            highest: highest_segment,
            files: candidates,
        } = directory.candidates()?;
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
        if log.published < required {
            return Err(Error::Prefix {
                recovered: log.published,
                required,
            });
        }
        log.issued = log.published;
        if let Some((index, offset)) = rejected {
            // Retain all rejected bytes before the first destructive change.
            for (relative, (number, file)) in candidates[index..].iter().enumerate() {
                let begin = if relative == 0 { offset } else { 0 };
                log.directory.archive(*number, file, begin)?;
                log.rejected_bytes += file.metadata()?.len() - begin;
            }
            for (relative, (number, file)) in candidates[index..].iter().enumerate() {
                if relative == 0 && offset != 0 {
                    file.set_len(offset)?;
                    file.sync_all()?;
                } else {
                    fs::remove_file(log.directory.path.join(segment::name(*number)))?;
                }
            }
            log.directory.sync()?;
        }
        // Sync the retained prefix before the new segment receives data. A fresh
        // epoch and recovery FENCE become durable before this handle can serve IO.
        for segment in &log.segments {
            direct::sync_data(&segment.file)?;
        }
        // The fresh segment ticket is also a unique, increasing epoch ticket.
        let epoch = highest_segment.checked_add(1).ok_or(Error::Exhausted)?;
        log.rotate(epoch)?;
        log.flush()?;
        Ok(log)
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
