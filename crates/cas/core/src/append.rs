//! Packed v2 staging, introduced separately from submission concurrency.
pub mod format;
mod index;
mod read;
mod recovery;
mod segment;
mod submission;

use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::sync::Arc;

use crate::{BLOCK_SIZE, MAX_REQUEST_BYTES, aligned::AlignedBuffer, direct};
use format::{Batch, Builder, Header, Kind, SegmentHeader};
use index::{Index, Mapping, Payload};
use segment::{Directory, Segment};

pub use crate::direct::Alignment;
pub use read::{ReadPlan, ReadRange};
pub use recovery::{LiveRecovery, Mutation, Recovery};
pub use submission::Submission;

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
    #[error("operation waits for earlier appends or an active sync cohort")]
    Pending,
    #[error("append requires a drained durable segment rollover")]
    Rollover,
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
    pub issued: u64,
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

/// Ordered staging state. The actual IO file descriptions stay locked and are
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
    issued: u64,
    pending_descriptors: usize,
    cohort: Option<submission::Cohort>,
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
            issued: 0,
            pending_descriptors: 0,
            cohort: None,
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
            issued: self.issued,
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

    pub fn config(&self) -> Config {
        self.config
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
        match self.check_append(&builder) {
            Err(Error::Rollover) => {
                self.flush()?;
                self.rollover()?;
            }
            result => result?,
        }
        let submission = self.prepare_append(builder)?;
        let result = submission.write();
        self.fail_on_io(result)?;
        self.publish_append(&submission)?;
        Ok(submission.into_batch())
    }

    pub fn flush(&mut self) -> Result<u64> {
        self.healthy()?;
        if self.cohort.is_some() || self.issued != self.published {
            return Err(Error::Pending);
        }
        if self.covers_flush(self.published) {
            return Ok(self.durable);
        }
        let fence = self.prepare_fence()?;
        let result = fence.write().and_then(|()| direct::sync_data(fence.file()));
        self.fail_on_io(result)?;
        self.complete_sync(&fence)
    }

    fn publish(&mut self, header: &Header<'_>, segment: Arc<Segment>, offset: u64) {
        let envelope = header.envelope();
        for descriptor in header.descriptors() {
            let source = (descriptor.kind == Kind::Write).then(|| {
                (
                    Arc::new(Payload {
                        segment: Arc::clone(&segment),
                        offset: offset + BLOCK_SIZE as u64 + u64::from(descriptor.payload_offset),
                        bytes: descriptor.payload_length as usize,
                        sequence: descriptor.sequence,
                        crc: descriptor.payload_crc,
                    }),
                    0,
                )
            });
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

    /// Reserve response plus MAX_REQUEST_BYTES checksum scratch before calling.
    pub fn read_into(&mut self, offset: u64, buffer: &mut AlignedBuffer) -> Result<()> {
        let plan = self.read_plan(offset, buffer.as_slice().len(), self.published)?;
        self.fail_on_io(plan.read_into(buffer))
    }
}

fn verify_read_crc(bytes: &[u8], crc: u32) -> io::Result<()> {
    if crc32fast::hash(bytes) != crc {
        return Err(io::Error::other("corrupt staging payload"));
    }
    Ok(())
}

#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
mod submission_tests;
#[cfg(test)]
mod tests;
