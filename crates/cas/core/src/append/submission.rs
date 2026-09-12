//! Physical reservations and ordered publication, shared by sync and async IO.
use std::fs::File;
use std::io;
use std::sync::Arc;

use super::{Error, Log, Result, Segment, format};
use crate::BLOCK_SIZE;
use format::{Batch, Builder, Header};

/// Ownership must survive the kernel operation. A CQE only establishes IO
/// completion; the caller publishes appends in order and syncs fences separately.
pub struct Submission {
    batch: Batch,
    segment: Arc<Segment>,
    offset: u64,
}

impl Submission {
    pub fn batch(&self) -> &Batch {
        &self.batch
    }
    pub fn file(&self) -> &File {
        &self.segment.file
    }
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Perform the full aligned write without publishing or synchronizing it.
    /// A short write fails; it is never retried as an unaligned suffix.
    pub fn write(&self) -> io::Result<()> {
        crate::direct::write_bytes(self.file(), self.batch.bytes(), self.offset)
    }
    pub fn into_batch(self) -> Batch {
        self.batch
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Cohort {
    segment: u64,
    batch: u64,
    boundary: u64,
}

impl From<&Submission> for Cohort {
    fn from(submission: &Submission) -> Self {
        let envelope = submission.batch.envelope();
        Self {
            segment: envelope.segment,
            batch: envelope.batch,
            boundary: envelope.last,
        }
    }
}

impl Log {
    pub fn covers_flush(&self, boundary: u64) -> bool {
        self.fenced && self.durable >= boundary
    }

    /// Check without consuming the final allocation. Pending and Rollover keep
    /// an admitted builder queued while older IO or its finite cohort drains.
    pub fn check_append(&self, builder: &Builder) -> Result<()> {
        self.healthy()?;
        if self.cohort.is_some() {
            return Err(Error::Pending);
        }
        if builder.is_empty() || builder.image_bytes() != self.config.image_bytes {
            return Err(format::Error::Invalid("empty append or builder image mismatch").into());
        }
        let growth = self
            .pending_descriptors
            .checked_add(builder.len())
            .and_then(|count| count.checked_mul(2))
            .and_then(|count| count.checked_add(self.index.len()));
        if growth.is_none_or(|count| count > self.limits.intervals) {
            return Err(Error::Capacity);
        }
        self.issued
            .checked_add(builder.len() as u64)
            .ok_or(Error::Exhausted)?;
        self.next_batch.checked_add(1).ok_or(Error::Exhausted)?;
        let bytes = (BLOCK_SIZE + builder.payload_bytes()) as u64;
        if self.offset + bytes + BLOCK_SIZE as u64 > self.config.segment_bytes {
            return Err(Error::Rollover);
        }
        Ok(())
    }

    pub fn prepare_append(&mut self, builder: Builder) -> Result<Submission> {
        self.check_append(&builder)?;
        let descriptors = builder.len();
        let batch = builder.seal(
            self.current().header.number,
            self.next_batch,
            self.issued + 1,
        )?;
        let submission = self.reserve(batch);
        self.issued = submission.batch.envelope().last;
        self.pending_descriptors += descriptors;
        Ok(submission)
    }

    fn reserve(&mut self, batch: Batch) -> Submission {
        let submission = Submission {
            batch,
            segment: Arc::clone(self.current()),
            offset: self.offset,
        };
        let bytes = submission.batch.bytes().len() as u64;
        self.offset += bytes;
        self.encoded_bytes += bytes;
        self.next_batch += 1; // callers check exhaustion before reserving
        submission
    }

    fn owns(&self, submission: &Submission) -> Result<()> {
        if !Arc::ptr_eq(self.current(), &submission.segment) {
            return Err(
                format::Error::Invalid("submission belongs to another writer or segment").into(),
            );
        }
        Ok(())
    }

    /// Call only after a full successful IO. Pending leaves the submission
    /// unchanged so an out-of-order CQE can wait for its predecessor.
    pub fn publish_append(&mut self, submission: &Submission) -> Result<()> {
        self.healthy()?;
        self.owns(submission)?;
        let header = Header::decode(
            &submission.batch.bytes()[..BLOCK_SIZE],
            self.config.image_bytes,
        )?;
        let envelope = header.envelope();
        if envelope.fence || envelope.first <= self.published {
            return Err(format::Error::Invalid("not an unpublished append").into());
        }
        if self.published.checked_add(1) != Some(envelope.first) {
            return Err(Error::Pending);
        }
        self.publish(&header, Arc::clone(&submission.segment), submission.offset);
        self.pending_descriptors -= envelope.descriptors;
        Ok(())
    }

    /// Freeze storage submission at the issued prefix, including outstanding
    /// append IO. The caller reserves a control buffer before invoking this.
    pub fn prepare_fence(&mut self) -> Result<Submission> {
        self.healthy()?;
        if self.cohort.is_some() {
            return Err(Error::Pending);
        }
        if self.offset + BLOCK_SIZE as u64 > self.config.segment_bytes {
            // A previously completed fence consumed the final reserved slot.
            self.rollover()?;
        }
        self.next_batch.checked_add(1).ok_or(Error::Exhausted)?;
        let fence = Batch::fence(self.current().header.number, self.next_batch, self.issued)?;
        let submission = self.reserve(fence);
        self.cohort = Some(Cohort::from(&submission));
        Ok(submission)
    }

    /// True only when covered appends have published. The reactor also waits
    /// for the fence write's CQE before submitting fdatasync.
    pub fn ready_to_sync(&self, fence: &Submission) -> Result<bool> {
        self.healthy()?;
        self.owns(fence)?;
        if !fence.batch.envelope().fence || self.cohort != Some(Cohort::from(fence)) {
            return Err(format::Error::Invalid("fence does not identify the active cohort").into());
        }
        Ok(self.pending_descriptors == 0 && self.published == fence.batch.envelope().last)
    }

    /// Call only after successful fdatasync of the fully completed cohort.
    pub fn complete_sync(&mut self, fence: &Submission) -> Result<u64> {
        if !self.ready_to_sync(fence)? {
            return Err(Error::Pending);
        }
        self.durable = fence.batch.envelope().last;
        self.fenced = true;
        self.cohort = None;
        Ok(self.durable)
    }

    pub fn rollover(&mut self) -> Result<()> {
        self.drained()?;
        self.rotate(Some(self.current().header.epoch))
    }

    /// Start a fresh frontend generation after its old published prefix is
    /// durable. The caller must sync a fence in the new segment before export.
    pub fn new_attachment(&mut self) -> Result<()> {
        self.drained()?;
        self.rotate(None)
    }

    fn drained(&self) -> Result<()> {
        self.healthy()?;
        if self.cohort.is_some()
            || self.pending_descriptors != 0
            || self.issued != self.published
            || self.durable != self.published
        {
            return Err(Error::Pending);
        }
        Ok(())
    }

    /// IO failure is terminal even when its CQE arrives before older successes.
    /// The daemon also marks its shared attachment FAILED under completion lock.
    pub fn fail(&mut self) {
        self.failed = true;
    }
}
