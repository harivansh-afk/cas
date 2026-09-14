//! Packed v2 staging, introduced separately from submission concurrency.
mod compaction;
pub mod format;
mod index;
mod read;
mod reclaim;
mod recovery;
mod rotation;
mod segment;
mod shared;
mod submission;

use allocator_api2::vec::Vec as BudgetVec;
use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::sync::Arc;

use crate::{
    BLOCK_SIZE, MAX_REQUEST_BYTES,
    aligned::AlignedBuffer,
    budget::{Amount, Budget, BudgetAllocator},
    direct,
};
use format::{Batch, Builder, Header, Kind, SegmentHeader};
use index::{Index, Mapping, Payload};
use segment::{Directory, Segment};

pub use crate::direct::Alignment;
pub use compaction::{Compacted, Input, Prepared as PreparedCompaction, Publication, Selection};
pub use read::{ReadPlan, ReadRange};
pub use reclaim::{ReclaimOperation, ReclaimStats, Reclaimed, Reclamation};
pub use recovery::{LivePlan, LiveRecovery, Mutation, Recovery};
pub use rotation::{Rotated, Rotation, RotationKind};
pub use shared::{SharedLivePlan, SharedRecovery};
pub use submission::{Position, Submission};

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
    pub fn validate(self, limits: Limits) -> Result<()> {
        self.header(1, 1, 0).validate()?;
        if self.segment_bytes < (format::MAX_BATCH_BYTES + 2 * BLOCK_SIZE) as u64
            || self.segment_bytes > limits.staging_bytes
            || limits.intervals < 2 * format::MAX_DESCRIPTORS
        {
            return Err(Error::Capacity);
        }
        Ok(())
    }

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
    pub compacted: u64,
    pub epoch: u64,
    pub segments: usize,
    pub intervals: usize,
    pub index_metadata_bytes: usize,
    pub segment_metadata_bytes: usize,
    pub segment_table_bytes: usize,
    pub read_pins: u32,
    pub index_nodes: usize,
    pub index_nodes_peak: usize,
    pub encoded_bytes: u64,
    pub allocated_bytes: u64,
    pub rejected_bytes: u64,
    pub alignment: Alignment,
    pub failed: bool,
    pub rotating: bool,
}

/// Ordered staging state. The actual IO file descriptions stay locked and are
/// shared with immutable payload pins. No explicit unlock precedes file release.
pub struct Log {
    directory: Directory,
    config: Config,
    limits: Limits,
    segments: BudgetVec<Arc<Segment>, BudgetAllocator>,
    index: Index,
    metadata: Arc<Budget>,
    offset: u64,
    next_batch: u64,
    highest_segment: u64,
    tickets: Option<Arc<crate::segments::Tickets>>,
    base: Option<crate::manifest::file::View>,
    compaction_cursor: Option<compaction::ScanPosition>,
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
    rotating: bool,
}

fn default_metadata() -> Arc<Budget> {
    Budget::new(Amount {
        bytes: 256 * MAX_REQUEST_BYTES,
        requests: 0,
    })
}

impl Log {
    pub fn manifest(&self) -> Option<&crate::manifest::file::View> {
        self.base.as_ref()
    }

    pub fn create(path: impl AsRef<Path>, config: Config, limits: Limits) -> Result<Self> {
        Self::create_with_metadata(path, config, limits, default_metadata())
    }

    /// Reserve all interval nodes from a shared budget before creating files.
    pub fn create_with_metadata(
        path: impl AsRef<Path>,
        config: Config,
        limits: Limits,
        metadata: Arc<Budget>,
    ) -> Result<Self> {
        Self::create_in(path.as_ref(), config, limits, metadata, None, None)
    }

    fn create_in(
        path: &Path,
        config: Config,
        limits: Limits,
        metadata: Arc<Budget>,
        tickets: Option<Arc<crate::segments::Tickets>>,
        base: Option<crate::manifest::file::View>,
    ) -> Result<Self> {
        config.validate(limits)?;
        let index = Index::new(limits.intervals, Arc::clone(&metadata))?;
        let pins = segment::Pins::new(config.segment_bytes, Arc::clone(&metadata))?;
        let mut segments = BudgetVec::new_in(BudgetAllocator::new(Arc::clone(&metadata)));
        segments.try_reserve_exact(1).map_err(|_| {
            io::Error::new(
                io::ErrorKind::OutOfMemory,
                "staging segment table exhausted",
            )
        })?;
        fs::create_dir(path)?;
        File::open(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?
        .sync_all()?;
        let directory = Directory::open(path)?;
        let segment = segment::create(&directory, config, tickets.as_deref(), 0, None, 0, pins)?;
        let highest_segment = segment.header.number;
        let allocated_bytes = segment.allocated_bytes()?;
        if allocated_bytes > limits.staging_bytes {
            return Err(Error::Capacity);
        }
        segments.push(segment);
        Ok(Self {
            directory,
            config,
            limits,
            segments,
            index,
            metadata,
            offset: BLOCK_SIZE as u64,
            next_batch: 1,
            highest_segment,
            tickets,
            base,
            compaction_cursor: None,
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
            rotating: false,
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
        let (index_nodes, index_nodes_peak) = self.index.nodes();
        Status {
            image_bytes: self.config.image_bytes,
            published: self.published,
            issued: self.issued,
            durable: self.durable,
            compacted: self.base.as_ref().map_or(0, |base| base.commit().durable),
            epoch: self.current().header.epoch,
            segments: self.segments.len(),
            intervals: self.index.len(),
            index_metadata_bytes: self.index.allocated_bytes(),
            segment_metadata_bytes: self.segments.iter().map(|s| s.pins.bytes()).sum(),
            segment_table_bytes: self.segments.capacity() * std::mem::size_of::<Arc<Segment>>(),
            read_pins: self.segments.iter().map(|s| s.pins.readers()).sum(),
            index_nodes,
            index_nodes_peak,
            encoded_bytes: self.encoded_bytes,
            allocated_bytes: self.allocated_bytes,
            rejected_bytes: self.rejected_bytes,
            alignment: self.current().alignment,
            failed: self.failed,
            rotating: self.rotating,
        }
    }

    pub fn config(&self) -> Config {
        self.config
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    pub fn uses_tickets(&self, tickets: &Arc<crate::segments::Tickets>) -> bool {
        self.tickets
            .as_ref()
            .is_some_and(|owner| Arc::ptr_eq(owner, tickets))
    }

    fn rotate(&mut self, epoch: Option<u64>) -> Result<()> {
        let prepared = self.rotation(epoch)?;
        let created = self.fail_on_io(prepared.create())?;
        self.install_rotation(created)
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
        let fence = match self.prepare_fence() {
            Err(Error::Rollover) => {
                self.rollover()?;
                self.prepare_fence()?
            }
            other => other?,
        };
        let result = fence.write().and_then(|()| direct::sync_data(fence.file()));
        self.fail_on_io(result)?;
        self.complete_sync(&fence)
    }

    fn publish(&mut self, header: &Header<'_>, segment: Arc<Segment>, offset: u64) {
        let envelope = header.envelope();
        for descriptor in header.descriptors() {
            let source = (descriptor.kind == Kind::Write).then(|| {
                (
                    Payload {
                        segment: Arc::clone(&segment),
                        offset: offset + BLOCK_SIZE as u64 + u64::from(descriptor.payload_offset),
                        bytes: descriptor.payload_length as usize,
                        sequence: descriptor.sequence,
                        crc: descriptor.payload_crc,
                        batch_block: (offset / BLOCK_SIZE as u64) as u32,
                    },
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
