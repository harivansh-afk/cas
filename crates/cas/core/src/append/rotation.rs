//! Sequenced preparation and installation around worker-owned allocation IO.
use super::*;
use crate::encoding::require;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RotationKind {
    Rollover,
    FreshAttachment,
}

struct Boundary {
    segment: Arc<Segment>,
    offset: u64,
    next_batch: u64,
    issued: u64,
    published: u64,
    durable: u64,
}

impl Boundary {
    fn matches(&self, log: &Log) -> Result<()> {
        require(
            log.rotating
                && Arc::ptr_eq(&self.segment, log.current())
                && self.offset == log.offset
                && self.next_batch == log.next_batch
                && self.issued == log.issued
                && self.published == log.published
                && self.durable == log.durable,
            "rotation no longer owns its sequenced boundary",
        )?;
        Ok(())
    }
}

/// A prepared rotation pins the actual old segment and directory. Dropping it
/// does not authorize append; explicitly cancel it or fail/recover the image.
pub struct Rotation {
    boundary: Boundary,
    directory: Directory,
    config: Config,
    tickets: Option<Arc<crate::segments::Tickets>>,
    highest: u64,
    epoch: Option<u64>,
    pins: segment::Pins,
}

/// Only successful creation plus file/directory sync can produce this receipt.
pub struct Rotated {
    boundary: Boundary,
    segment: Arc<Segment>,
    allocated: u64,
}

impl Log {
    pub fn prepare_rotation(&mut self, kind: RotationKind) -> Result<Rotation> {
        self.drained()?;
        self.rotation(match kind {
            RotationKind::Rollover => Some(self.current().header.epoch),
            RotationKind::FreshAttachment => None,
        })
    }

    // Recovery has synced the retained files but has not established E yet.
    // Its private path shares creation/installation without claiming E early.
    pub(super) fn rotation(&mut self, epoch: Option<u64>) -> Result<Rotation> {
        self.healthy()?;
        if self.rotating || self.cohort.is_some() || self.issued != self.published {
            return Err(Error::Pending);
        }
        if self
            .allocated_bytes
            .checked_add(self.config.segment_bytes)
            .is_none_or(|bytes| bytes > self.limits.staging_bytes)
        {
            return Err(Error::Capacity);
        }
        self.encoded_bytes
            .checked_add(BLOCK_SIZE as u64)
            .ok_or(Error::Exhausted)?;
        let pins = segment::Pins::new(self.config.segment_bytes, Arc::clone(&self.metadata))?;
        self.segments.try_reserve(1).map_err(|_| {
            io::Error::new(
                io::ErrorKind::OutOfMemory,
                "staging segment table exhausted",
            )
        })?;
        let directory = self.directory.duplicate()?;
        let prepared = Rotation {
            boundary: Boundary {
                segment: Arc::clone(self.current()),
                offset: self.offset,
                next_batch: self.next_batch,
                issued: self.issued,
                published: self.published,
                durable: self.durable,
            },
            directory,
            config: self.config,
            tickets: self.tickets.clone(),
            highest: self.highest_segment,
            epoch,
            pins,
        };
        self.rotating = true;
        Ok(prepared)
    }

    pub fn cancel_rotation(&mut self, prepared: Rotation) -> Result<()> {
        self.healthy()?;
        prepared.boundary.matches(self)?;
        self.rotating = false;
        Ok(())
    }

    /// Under the image sequencer after successful worker IO. No syscall occurs.
    pub fn install_rotation(&mut self, created: Rotated) -> Result<()> {
        self.healthy()?;
        created.boundary.matches(self)?;
        let allocated = self
            .allocated_bytes
            .checked_add(created.allocated)
            .filter(|bytes| *bytes <= self.limits.staging_bytes);
        let Some(allocated) = allocated else {
            self.failed = true;
            return Err(Error::Capacity);
        };
        self.highest_segment = created.segment.header.number;
        self.allocated_bytes = allocated;
        self.segments.push(created.segment);
        self.offset = BLOCK_SIZE as u64;
        self.next_batch = 1;
        self.fenced = false;
        self.rotating = false;
        self.encoded_bytes += BLOCK_SIZE as u64;
        Ok(())
    }
}

impl Rotation {
    pub fn segment_bytes(&self) -> u64 {
        self.config.segment_bytes
    }

    /// Run on the allocation worker. This owns every file through sync, even
    /// after the reactor has failed or its caller's deadline has elapsed.
    pub fn create(self) -> io::Result<Rotated> {
        let segment = segment::create(
            &self.directory,
            self.config,
            self.tickets.as_deref(),
            self.highest,
            self.epoch,
            self.boundary.published,
            self.pins,
        )?;
        let allocated = segment.allocated_bytes()?;
        Ok(Rotated {
            boundary: self.boundary,
            segment,
            allocated,
        })
    }
}
