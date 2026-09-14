//! Bounded background IO and sequenced publication share one compaction receipt.
mod output;
pub use output::{Prepared, Publication};

use super::{
    Log, Result,
    format::{Header, Kind},
    segment::Segment,
};
use crate::{
    BLOCK_SIZE, MAX_REQUEST_BYTES,
    aligned::AlignedBuffer,
    budget::{Budget, BudgetAllocator},
    direct,
    encoding::require,
    manifest::{file::View, tree::MAX_CHANGES},
};
use allocator_api2::vec::Vec;
use arrayvec::ArrayVec;
use std::{
    io,
    sync::{Arc, atomic::Ordering},
};

/// Volatile hint, accepted only for the published D and a retained segment.
/// Framing is still decoded and validated from this exact batch boundary.
#[derive(Clone, Copy)]
pub(super) struct ScanPosition {
    pub segment: u64,
    pub offset: u64,
    pub batch: u64,
    pub sequence: u64,
}

pub(super) struct Span {
    pub segment: Arc<Segment>,
    pub end: u64,
    protect: bool,
}

impl Drop for Span {
    fn drop(&mut self) {
        if self.protect {
            self.segment.pins.release_scan();
        }
    }
}

pub(super) fn spans(
    log: &Log,
    metadata: Arc<Budget>,
    protect: bool,
) -> io::Result<Vec<Span, BudgetAllocator>> {
    let mut spans = Vec::new_in(BudgetAllocator::new(metadata));
    spans.try_reserve_exact(log.segments.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::OutOfMemory,
            "compaction segment metadata exhausted",
        )
    })?;
    for segment in &log.segments {
        if protect {
            segment.pins.hold_scan();
        }
        spans.push(Span {
            segment: Arc::clone(segment),
            end: segment.end.load(Ordering::Relaxed),
            protect,
        });
    }
    Ok(spans)
}

/// No IO at capture. E and immutable end snapshots bound the background scan.
pub struct Selection {
    base: View,
    cursor: Option<ScanPosition>,
    through: u64,
    spans: Vec<Span, BudgetAllocator>,
    metadata: Arc<Budget>,
    io_memory: Arc<Budget>,
}

impl Log {
    /// The host already owns the one compactor slot and output-space promise.
    pub fn select_compaction(
        &self,
        metadata: Arc<Budget>,
        io_memory: Arc<Budget>,
    ) -> Result<Option<Selection>> {
        self.healthy()?;
        let base = self
            .base
            .as_ref()
            .ok_or_else(|| io::Error::other("compaction requires a manifest-backed log"))?;
        if base.commit().durable == self.durable {
            return Ok(None);
        }
        require(
            base.commit().durable < self.durable,
            "manifest D exceeds durable WAL",
        )?;
        Ok(Some(Selection {
            base: base.clone(),
            cursor: self.compaction_cursor.filter(|cursor| {
                cursor.sequence == base.commit().durable
                    && self.segments.iter().any(|segment| {
                        segment.header.number == cursor.segment
                            && cursor.offset <= segment.end.load(Ordering::Relaxed)
                    })
            }),
            through: self.durable,
            spans: spans(self, Arc::clone(&metadata), true)?,
            metadata,
            io_memory,
        }))
    }

    /// Hold the image completion/FAILED gate through this sequencer operation.
    pub fn publish_compaction(&mut self, compacted: Compacted) -> Result<()> {
        self.healthy()?;
        let base = self
            .base
            .as_ref()
            .ok_or_else(|| io::Error::other("missing compaction base"))?;
        let durable = compacted.view.commit().durable;
        require(
            base.same(&compacted.previous)
                && durable > base.commit().durable
                && durable <= self.durable,
            "stale or non-durable compaction receipt",
        )?;
        require(
            compacted.cursor.sequence == durable,
            "compaction cursor differs from D",
        )?;
        self.index.retain_after(durable);
        self.compaction_cursor = Some(compacted.cursor);
        self.base = Some(compacted.view);
        Ok(())
    }
}

struct Edit {
    start: u64,
    end: u64,
    payload: Option<usize>,
}

/// Owned original bytes; no mutable staging map is copied or borrowed by IO.
pub struct Input {
    base: View,
    cursor: Option<ScanPosition>,
    through: u64,
    payload: AlignedBuffer<BudgetAllocator>,
    payload_bytes: usize,
    edits: ArrayVec<Edit, MAX_CHANGES>,
    metadata: Arc<Budget>,
}

impl Selection {
    pub fn load(self) -> io::Result<Input> {
        let image_bytes = self.base.commit().image_bytes;
        let base = self.base.commit().durable;
        let mut input = Input {
            base: self.base,
            cursor: self.cursor,
            through: base,
            payload: AlignedBuffer::try_new_in(
                MAX_REQUEST_BYTES,
                BudgetAllocator::new(self.io_memory),
            )?,
            payload_bytes: 0,
            edits: ArrayVec::new(),
            metadata: Arc::clone(&self.metadata),
        };
        let mut scratch =
            AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(self.metadata))?;
        let first = self
            .spans
            .first()
            .ok_or_else(|| io::Error::other("no compaction segments"))?;
        let mut sequence = self
            .cursor
            .map_or(first.segment.header.preceding_sequence, |cursor| {
                cursor.sequence
            });
        require(sequence <= base, "compaction starts above D")?;
        let mut bounded = false;
        'segments: for span in self.spans {
            if self
                .cursor
                .is_some_and(|cursor| span.segment.header.number < cursor.segment)
            {
                continue;
            }
            let resume = self
                .cursor
                .filter(|cursor| span.segment.header.number == cursor.segment);
            require(
                resume.is_some() || span.segment.header.preceding_sequence == sequence,
                "compaction segment gap",
            )?;
            let mut offset = resume.map_or(BLOCK_SIZE as u64, |cursor| cursor.offset);
            let mut batch = resume.map_or(1, |cursor| cursor.batch);
            while offset < span.end {
                if sequence == self.through {
                    break 'segments;
                }
                direct::read_bytes(&span.segment.file, scratch.as_mut_slice(), offset)?;
                let header =
                    Header::decode(scratch.as_slice(), image_bytes).map_err(io::Error::other)?;
                require(
                    header.follows(span.segment.header, batch, sequence, offset, span.end),
                    "invalid durable compaction batch",
                )?;
                let envelope = header.envelope();
                if !envelope.fence {
                    if envelope.last > base {
                        require(
                            envelope.first > base && envelope.last <= self.through,
                            "compaction cut splits a batch",
                        )?;
                        let edits: usize = header
                            .descriptors()
                            .map(|d| {
                                if d.kind == Kind::Write {
                                    d.payload_length as usize / BLOCK_SIZE
                                } else {
                                    1
                                }
                            })
                            .sum();
                        if input.edits.len() + edits > MAX_CHANGES
                            || input.payload_bytes + envelope.payload_bytes > MAX_REQUEST_BYTES
                        {
                            bounded = true;
                            break 'segments;
                        }
                        let data = &mut input.payload.as_mut_slice()
                            [input.payload_bytes..input.payload_bytes + envelope.payload_bytes];
                        if !data.is_empty() {
                            direct::read_bytes(
                                &span.segment.file,
                                data,
                                offset + BLOCK_SIZE as u64,
                            )?;
                        }
                        header.verify_payload(data).map_err(io::Error::other)?;
                        for descriptor in header.descriptors() {
                            let start = descriptor.offset / BLOCK_SIZE as u64;
                            if descriptor.kind == Kind::Zero {
                                input.edits.push(Edit {
                                    start,
                                    end: start + descriptor.length / BLOCK_SIZE as u64,
                                    payload: None,
                                });
                            } else {
                                for block in 0..descriptor.payload_length as usize / BLOCK_SIZE {
                                    input.edits.push(Edit {
                                        start: start + block as u64,
                                        end: start + block as u64 + 1,
                                        payload: Some(
                                            input.payload_bytes
                                                + descriptor.payload_offset as usize
                                                + block * BLOCK_SIZE,
                                        ),
                                    });
                                }
                            }
                        }
                        input.payload_bytes += envelope.payload_bytes;
                        input.through = envelope.last;
                    }
                    sequence = envelope.last;
                }
                offset += (BLOCK_SIZE + envelope.payload_bytes) as u64;
                batch = batch
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("compaction batch overflow"))?;
                input.cursor = Some(ScanPosition {
                    segment: span.segment.header.number,
                    offset,
                    batch,
                    sequence,
                });
            }
        }
        require(
            bounded || sequence == self.through,
            "compaction scan ended below E",
        )?;
        require(
            input.through > base,
            "durable compaction prefix is unavailable",
        )?;
        Ok(input)
    }
}

impl Input {
    pub fn through(&self) -> u64 {
        self.through
    }
    pub fn payload_bytes(&self) -> usize {
        self.payload_bytes
    }
    pub fn edits(&self) -> usize {
        self.edits.len()
    }
}

/// Constructible only after verified chunks and a successful manifest sync.
pub struct Compacted {
    previous: View,
    cursor: ScanPosition,
    view: View,
}

impl Compacted {
    pub fn durable(&self) -> u64 {
        self.view.commit().durable
    }
}
