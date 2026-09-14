//! Background reclamation retains directory/segment ownership through its IO.
use super::{
    Log, Result,
    compaction::{Span, spans},
    format::Header,
    segment,
};
use crate::{
    BLOCK_SIZE,
    aligned::AlignedBuffer,
    budget::{Budget, BudgetAllocator},
    direct,
    directory::Directory,
    encoding::require,
    manifest::file::View,
    segments::Tickets,
};
use allocator_api2::vec::Vec;
use std::{fs, io, os::unix::fs::MetadataExt, sync::Arc};

#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct ReclaimStats {
    /// Bytes passed to successful punch calls; repeated holes are not physical progress.
    pub punch_requested_bytes: u64,
    pub pinned_batches: u64,
    pub removed_segments: usize,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(tag = "operation", rename_all = "lowercase")]
pub enum ReclaimOperation {
    Punch {
        segment: u64,
        offset: u64,
        bytes: u64,
    },
    Unlink {
        segment: u64,
    },
}

pub struct Reclamation {
    base: View,
    directory: Directory,
    tickets: Arc<Tickets>,
    spans: Vec<Span, BudgetAllocator>,
    current: u64,
    oldest_live: Option<u64>,
    metadata: Arc<Budget>,
}

struct Change {
    number: u64,
    before: u64,
    after: u64,
    removed: bool,
}

/// Physical IO completed. Apply under the sequencer before releasing its slot.
pub struct Reclaimed {
    base: View,
    changes: Vec<Change, BudgetAllocator>,
    stats: ReclaimStats,
}

impl Log {
    /// oldest_live is the earliest mutation whose original identity is still
    /// needed by retained inflight recovery, or None when all have retired.
    /// Admission can precede Log assignment, so this cutoff may exceed issued.
    pub fn select_reclamation(
        &self,
        oldest_live: Option<u64>,
        metadata: Arc<Budget>,
    ) -> Result<Reclamation> {
        self.healthy()?;
        let base = self
            .base
            .as_ref()
            .ok_or_else(|| io::Error::other("reclamation requires a manifest"))?;
        require(
            oldest_live.is_none_or(|sequence| sequence != 0),
            "invalid oldest live mutation",
        )?;
        Ok(Reclamation {
            base: base.clone(),
            directory: self.directory.duplicate()?,
            tickets: Arc::clone(
                self.tickets
                    .as_ref()
                    .ok_or_else(|| io::Error::other("reclamation requires shared tickets"))?,
            ),
            spans: spans(self, Arc::clone(&metadata), false)?,
            current: self.current().header.number,
            oldest_live,
            metadata,
        })
    }

    pub fn apply_reclamation(&mut self, reclaimed: Reclaimed) -> Result<ReclaimStats> {
        self.healthy()?;
        require(
            self.base
                .as_ref()
                .is_some_and(|base| base.same(&reclaimed.base)),
            "reclamation base changed",
        )?;
        for change in &reclaimed.changes {
            if change.removed {
                require(
                    change.number != self.current().header.number,
                    "cannot remove current staging segment",
                )?;
                self.segments
                    .retain(|segment| segment.header.number != change.number);
            }
            self.allocated_bytes = self
                .allocated_bytes
                .checked_sub(change.before)
                .and_then(|bytes| bytes.checked_add(change.after))
                .ok_or_else(|| io::Error::other("staging allocation accounting diverged"))?;
        }
        Ok(reclaimed.stats)
    }
}

impl Reclamation {
    /// Run on the single allocation/background owner. The filesystem governor
    /// reconciles actual free space after applying/dropping all retired owners;
    /// an unlink is not a promise of instant physical block release.
    pub fn run(self) -> io::Result<Reclaimed> {
        self.run_with(|_| Ok(()))
    }

    pub fn run_with(
        self,
        mut observe: impl FnMut(ReclaimOperation) -> io::Result<()>,
    ) -> io::Result<Reclaimed> {
        let mut changes = Vec::new_in(BudgetAllocator::new(Arc::clone(&self.metadata)));
        changes.try_reserve_exact(self.spans.len()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::OutOfMemory,
                "reclamation result metadata exhausted",
            )
        })?;
        let mut scratch =
            AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(self.metadata))?;
        let mut stats = ReclaimStats::default();
        let durable = self.base.commit().durable;
        for span in self.spans {
            let segment = &span.segment;
            if segment.header.preceding_sequence > durable {
                continue;
            }
            let before = segment.allocated_bytes()?;
            let mut offset = BLOCK_SIZE as u64;
            let mut batch = 1;
            let mut sequence = segment.header.preceding_sequence;
            let mut punched = false;
            while offset < span.end {
                if segment.header.number == self.current && sequence == durable {
                    break;
                }
                direct::read_bytes(&segment.file, scratch.as_mut_slice(), offset)?;
                let header = Header::decode(scratch.as_slice(), self.base.commit().image_bytes)
                    .map_err(io::Error::other)?;
                require(
                    header.follows(segment.header, batch, sequence, offset, span.end),
                    "invalid covered staging batch",
                )?;
                let envelope = header.envelope();
                if envelope.last > durable {
                    require(
                        envelope.fence || envelope.first > durable,
                        "reclamation cut splits a batch",
                    )?;
                    break;
                }
                if !envelope.fence {
                    sequence = envelope.last;
                    if envelope.payload_bytes != 0 {
                        if segment.pins.at(0) == 0
                            && segment.pins.at((offset / BLOCK_SIZE as u64) as u32) == 0
                        {
                            direct::punch(
                                &segment.file,
                                offset + BLOCK_SIZE as u64,
                                envelope.payload_bytes as u64,
                            )?;
                            observe(ReclaimOperation::Punch {
                                segment: segment.header.number,
                                offset: offset + BLOCK_SIZE as u64,
                                bytes: envelope.payload_bytes as u64,
                            })?;
                            stats.punch_requested_bytes += envelope.payload_bytes as u64;
                            punched = true;
                        } else {
                            stats.pinned_batches += 1;
                        }
                    }
                }
                offset += (BLOCK_SIZE + envelope.payload_bytes) as u64;
                batch = batch
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("reclamation batch overflow"))?;
            }
            if punched {
                direct::sync_data(&segment.file)?;
            }
            // The log and this span are the only segment owners. Also exclude
            // separately retained actual-file references before removing a name.
            let removed = offset == span.end
                && sequence <= durable
                && segment.header.number != self.current
                && segment.header.number < self.tickets.status().highest
                && self.oldest_live.is_none_or(|oldest| sequence < oldest)
                && segment.pins.readers() == 0
                && Arc::strong_count(segment) == 2
                && Arc::strong_count(&segment.file) == 1;
            let after = if removed {
                let unlink = crate::io_metrics::measure(0, |c| &mut c.unlink);
                fs::remove_file(
                    self.directory
                        .path
                        .join(segment::name(segment.header.number)),
                )?;
                drop(unlink);
                observe(ReclaimOperation::Unlink {
                    segment: segment.header.number,
                })?;
                self.directory.sync()?;
                stats.removed_segments += 1;
                0
            } else {
                segment.file.metadata()?.blocks() * 512
            };
            require(after <= before, "reclamation grew staging allocation")?;
            changes.push(Change {
                number: segment.header.number,
                before,
                after,
                removed,
            });
        }
        Ok(Reclaimed {
            base: self.base,
            changes,
            stats,
        })
    }
}
