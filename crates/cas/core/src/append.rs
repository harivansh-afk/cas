//! Packed v2 staging, introduced separately from submission concurrency.
pub mod format;
mod index;
mod recovery;
mod segment;

use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::sync::Arc;

use crate::{BLOCK_SIZE, MAX_REQUEST_BYTES, aligned::AlignedBuffer, direct};
use format::{Batch, Builder, Header, Kind, SegmentHeader};
use index::{Index, Mapping, Payload};
use segment::{Directory, Segment};

pub use crate::direct::Alignment;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Format(#[from] format::Error),
    #[error("append writer stopped after an IO failure")]
    Failed,
    #[error("append identifier exhausted")]
    Exhausted,
    #[error("staging capacity exhausted")]
    Capacity,
    #[error("recovery prefix {recovered} does not cover required prefix {required}")]
    Prefix { recovered: u64, required: u64 },
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub store: [u8; 16],
    pub image: [u8; 16],
    pub image_bytes: u64,
    pub segment_bytes: u64,
}

impl Config {
    fn header(self, epoch: u64, number: u64, preceding_sequence: u64) -> SegmentHeader {
        SegmentHeader {
            store: self.store,
            image: self.image,
            epoch,
            number,
            capacity: self.segment_bytes,
            image_bytes: self.image_bytes,
            preceding_sequence,
        }
    }

    fn matches(self, header: SegmentHeader) -> bool {
        self.store == header.store
            && self.image == header.image
            && self.image_bytes == header.image_bytes
            && self.segment_bytes == header.capacity
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub staging_bytes: u64,
    pub intervals: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            staging_bytes: 256 * MAX_REQUEST_BYTES as u64,
            intervals: 65536,
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Status {
    pub image_bytes: u64,
    pub published: u64,
    pub durable: u64,
    pub epoch: u64,
    pub segments: usize,
    pub intervals: usize,
    pub encoded_bytes: u64,
    pub allocated_bytes: u64,
    pub rejected_bytes: u64,
    pub alignment: Alignment,
    pub failed: bool,
}

/// C2 serial execution. The actual IO file descriptions stay locked and are
/// shared with immutable payload pins. No explicit unlock precedes file release.
pub struct Log {
    directory: Directory,
    config: Config,
    limits: Limits,
    segments: Vec<Arc<Segment>>,
    index: Index,
    offset: u64,
    next_batch: u64,
    highest_segment: u64,
    published: u64,
    durable: u64,
    encoded_bytes: u64,
    allocated_bytes: u64,
    rejected_bytes: u64,
    failed: bool,
    fenced: bool,
}

impl Log {
    pub fn create(path: impl AsRef<Path>, config: Config, limits: Limits) -> Result<Self> {
        let path = path.as_ref();
        config.header(1, 1, 0).encode()?;
        if config.segment_bytes < (format::MAX_BATCH_BYTES + 2 * BLOCK_SIZE) as u64
            || config.segment_bytes > limits.staging_bytes
            || limits.intervals < 2 * format::MAX_DESCRIPTORS
        {
            return Err(Error::Capacity);
        }
        fs::create_dir(path)?;
        File::open(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?
        .sync_all()?;
        let directory = Directory::open(path)?;
        let segment = Segment::create(&directory, config.header(1, 1, 0))?;
        let allocated_bytes = segment.allocated_bytes()?;
        if allocated_bytes > limits.staging_bytes {
            return Err(Error::Capacity);
        }
        Ok(Self {
            directory,
            config,
            limits,
            segments: vec![segment],
            index: Index::default(),
            offset: BLOCK_SIZE as u64,
            next_batch: 1,
            highest_segment: 1,
            published: 0,
            durable: 0,
            encoded_bytes: BLOCK_SIZE as u64,
            allocated_bytes,
            rejected_bytes: 0,
            failed: false,
            fenced: false,
        })
    }

    fn current(&self) -> &Arc<Segment> {
        self.segments
            .last()
            .expect("a log owns its current segment")
    }

    fn healthy(&self) -> Result<()> {
        if self.failed {
            Err(Error::Failed)
        } else {
            Ok(())
        }
    }

    fn fail_on_io<T>(&mut self, result: io::Result<T>) -> Result<T> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                self.failed = true;
                Err(error.into())
            }
        }
    }

    pub fn status(&self) -> Status {
        Status {
            image_bytes: self.config.image_bytes,
            published: self.published,
            durable: self.durable,
            epoch: self.current().header.epoch,
            segments: self.segments.len(),
            intervals: self.index.len(),
            encoded_bytes: self.encoded_bytes,
            allocated_bytes: self.allocated_bytes,
            rejected_bytes: self.rejected_bytes,
            alignment: self.current().alignment,
            failed: self.failed,
        }
    }

    fn rotate(&mut self, epoch: u64) -> Result<()> {
        let number = self
            .highest_segment
            .checked_add(1)
            .ok_or(Error::Exhausted)?;
        if self
            .allocated_bytes
            .checked_add(self.config.segment_bytes)
            .is_none_or(|bytes| bytes > self.limits.staging_bytes)
        {
            return Err(Error::Capacity);
        }
        let created = Segment::create(
            &self.directory,
            self.config.header(epoch, number, self.published),
        );
        let segment = self.fail_on_io(created)?;
        self.highest_segment = number;
        self.allocated_bytes += segment.allocated_bytes()?;
        if self.allocated_bytes > self.limits.staging_bytes {
            self.failed = true;
            return Err(Error::Capacity);
        }
        self.segments.push(segment);
        self.offset = BLOCK_SIZE as u64;
        self.next_batch = 1;
        self.fenced = false;
        self.encoded_bytes += BLOCK_SIZE as u64;
        Ok(())
    }

    pub fn append(&mut self, builder: Builder) -> Result<Batch> {
        self.healthy()?;
        if builder.is_empty() {
            return Err(format::Error::Invalid("empty append").into());
        }
        if builder.image_bytes() != self.config.image_bytes {
            return Err(format::Error::Invalid("builder image mismatch").into());
        }
        // An overwrite can split both boundary intervals. Reserve the maximum
        // growth for every descriptor before issuing payload IO.
        if self
            .index
            .len()
            .checked_add(2 * builder.len())
            .is_none_or(|count| count > self.limits.intervals)
        {
            return Err(Error::Capacity);
        }
        let first = self.published.checked_add(1).ok_or(Error::Exhausted)?;
        self.published
            .checked_add(builder.len() as u64)
            .ok_or(Error::Exhausted)?;
        let bytes = (BLOCK_SIZE + builder.payload_bytes()) as u64;
        if self.offset + bytes + BLOCK_SIZE as u64 > self.config.segment_bytes {
            self.flush()?;
            self.rotate(self.current().header.epoch)?;
        }
        let batch = builder.seal(self.current().header.number, self.next_batch, first)?;
        self.next_batch.checked_add(1).ok_or(Error::Exhausted)?;
        let result = direct::write_bytes(&self.current().file, batch.bytes(), self.offset);
        self.fail_on_io(result)?;
        let header = Header::decode(&batch.bytes()[..BLOCK_SIZE], self.config.image_bytes)?;
        self.publish(&header, Arc::clone(self.current()), self.offset);
        self.offset += bytes;
        self.encoded_bytes += bytes;
        self.next_batch += 1;
        Ok(batch)
    }

    pub fn flush(&mut self) -> Result<u64> {
        self.healthy()?;
        if self.fenced && self.durable == self.published {
            return Ok(self.durable);
        }
        self.next_batch.checked_add(1).ok_or(Error::Exhausted)?;
        if self.offset + BLOCK_SIZE as u64 > self.config.segment_bytes {
            // Only a preceding fence can consume the reserved final slot.
            // Synchronize it before rotating, then emit the requested new fence.
            let result = direct::sync_data(&self.current().file);
            self.fail_on_io(result)?;
            self.rotate(self.current().header.epoch)?;
        }
        let fence = Batch::fence(
            self.current().header.number,
            self.next_batch,
            self.published,
        )?;
        let result = direct::write_bytes(&self.current().file, fence.bytes(), self.offset)
            .and_then(|()| direct::sync_data(&self.current().file));
        self.fail_on_io(result)?;
        self.offset += BLOCK_SIZE as u64;
        self.encoded_bytes += BLOCK_SIZE as u64;
        self.next_batch += 1;
        self.durable = self.published;
        self.fenced = true;
        Ok(self.durable)
    }

    fn publish(&mut self, header: &Header<'_>, segment: Arc<Segment>, offset: u64) {
        let envelope = header.envelope();
        let payload = Arc::new(Payload {
            segment,
            offset: offset + BLOCK_SIZE as u64,
            bytes: envelope.payload_bytes,
            first: envelope.first,
            last: envelope.last,
        });
        for descriptor in header.descriptors() {
            let source = (descriptor.kind == Kind::Write)
                .then(|| (Arc::clone(&payload), u64::from(descriptor.payload_offset)));
            self.index.replace(
                descriptor.offset,
                Mapping {
                    end: descriptor.offset + descriptor.length,
                    sequence: descriptor.sequence,
                    source,
                },
            );
        }
        self.published = envelope.last;
    }

    pub fn read_into(&mut self, offset: u64, buffer: &mut AlignedBuffer) -> Result<()> {
        self.healthy()?;
        let length = buffer.as_slice().len();
        let end = offset.checked_add(length as u64).ok_or(Error::Exhausted)?;
        if !offset.is_multiple_of(BLOCK_SIZE as u64)
            || end > self.config.image_bytes
            || length > MAX_REQUEST_BYTES
        {
            return Err(format::Error::Invalid("read range").into());
        }
        buffer.as_mut_slice().fill(0);
        for (begin, mapping) in self.index.overlapping(offset, end) {
            let Some((payload, payload_offset)) = &mapping.source else {
                continue;
            };
            let first = begin.max(offset);
            let last = mapping.end.min(end);
            let skip = payload_offset + first - begin;
            debug_assert!(skip + last - first <= payload.bytes as u64);
            debug_assert!((payload.first..=payload.last).contains(&mapping.sequence));
            if let Err(error) = direct::read_bytes(
                &payload.segment.file,
                &mut buffer.as_mut_slice()[(first - offset) as usize..(last - offset) as usize],
                payload.offset + skip,
            ) {
                self.failed = true;
                return Err(error.into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
