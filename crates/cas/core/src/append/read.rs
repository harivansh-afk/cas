//! Immutable read snapshots. Payload pins retain the actual locked IO files.
use crate::budget::BudgetAllocator;
use allocator_api2::vec::Vec;
use std::fs::File;
use std::io;
use std::ops::Range;
use std::sync::Arc;

use super::{Error, Log, Result, format, index::Payload, verify_read_crc};
use crate::{BLOCK_SIZE, MAX_REQUEST_BYTES, aligned::AlignedBuffer, direct};

pub struct ReadRange {
    payload: Payload,
    source: Range<usize>,
    destination: Range<usize>,
}

impl Drop for ReadRange {
    fn drop(&mut self) {
        self.payload.segment.pins.release(self.payload.batch_block);
    }
}

impl ReadRange {
    pub fn file(&self) -> &File {
        &self.payload.segment.file
    }
    pub fn offset(&self) -> u64 {
        self.payload.offset
    }
    pub fn input_bytes(&self) -> usize {
        self.payload.bytes
    }
    pub fn source(&self) -> Range<usize> {
        self.source.clone()
    }
    pub fn destination(&self) -> Range<usize> {
        self.destination.clone()
    }
    pub fn direct_to_response(&self) -> bool {
        self.source == (0..self.payload.bytes)
    }

    pub fn verify(&self, input: &[u8]) -> io::Result<()> {
        if input.len() != self.payload.bytes {
            return Err(io::Error::other("short staging payload read"));
        }
        verify_read_crc(input, self.payload.crc)
    }
}

pub struct ReadPlan {
    offset: u64,
    bytes: usize,
    ranges: Vec<ReadRange, BudgetAllocator>,
    covered: [u64; 4],
    base: Option<crate::manifest::file::View>,
}

impl ReadPlan {
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn ranges(&self) -> &[ReadRange] {
        &self.ranges
    }

    /// Relative block coverage includes explicit ZERO and staged payload alike.
    pub fn staged(&self, block: usize) -> bool {
        assert!(block < self.bytes / BLOCK_SIZE);
        self.covered[block / 64] & (1 << (block % 64)) != 0
    }
    pub fn manifest(&self) -> Option<&crate::manifest::file::View> {
        self.base.as_ref()
    }
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Synchronous reference execution, using the same immutable plan as the
    /// reactor. The caller reserves response plus at most 1 MiB scratch bytes.
    pub fn read_into(&self, buffer: &mut AlignedBuffer) -> io::Result<()> {
        self.read_with(buffer, |_, _| {
            Err(io::Error::other(
                "manifest chunk requires a verified store loader",
            ))
        })
    }

    /// Fill uncovered blocks through the captured manifest and a verified chunk
    /// loader. The async caller drives these same pins/coverage through CQEs.
    pub fn read_with(
        &self,
        buffer: &mut AlignedBuffer,
        mut load: impl FnMut(crate::chunk_index::Hash, &mut [u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        if buffer.as_slice().len() != self.bytes {
            return Err(io::Error::other("read response length differs from plan"));
        }
        buffer.as_mut_slice().fill(0);
        let mut scratch: Option<AlignedBuffer> = None;
        for range in &self.ranges {
            let destination = &mut buffer.as_mut_slice()[range.destination()];
            if range.direct_to_response() {
                direct::read_bytes(range.file(), destination, range.offset())?;
                range.verify(destination)?;
            } else {
                if scratch
                    .as_ref()
                    .is_none_or(|buffer| buffer.as_slice().len() < range.input_bytes())
                {
                    drop(scratch.take());
                    scratch = Some(AlignedBuffer::new(range.input_bytes()));
                }
                let input = &mut scratch.as_mut().unwrap().as_mut_slice()[..range.input_bytes()];
                direct::read_bytes(range.file(), input, range.offset())?;
                range.verify(input)?;
                destination.copy_from_slice(&input[range.source()]);
            }
        }
        if let Some(base) = &self.base
            && (0..self.bytes / BLOCK_SIZE).any(|block| !self.staged(block))
        {
            let mut tree = base.tree()?;
            for block in 0..self.bytes / BLOCK_SIZE {
                if !self.staged(block)
                    && let Some(hash) = tree.get(self.offset / BLOCK_SIZE as u64 + block as u64)?
                {
                    load(
                        hash,
                        &mut buffer.as_mut_slice()[block * BLOCK_SIZE..(block + 1) * BLOCK_SIZE],
                    )?;
                }
            }
        }
        Ok(())
    }
}

impl Log {
    /// Capture only after the admitted mutation boundary has published. Later
    /// overwrites cannot change these immutable payload references.
    pub fn read_plan(&self, offset: u64, bytes: usize, boundary: u64) -> Result<ReadPlan> {
        self.healthy()?;
        let end = offset.checked_add(bytes as u64).ok_or(Error::Exhausted)?;
        if bytes == 0
            || !offset.is_multiple_of(BLOCK_SIZE as u64)
            || !bytes.is_multiple_of(BLOCK_SIZE)
            || end > self.config.image_bytes
            || bytes > MAX_REQUEST_BYTES
        {
            return Err(format::Error::Invalid("read range").into());
        }
        if self.published < boundary {
            return Err(Error::Pending);
        }
        let mut ranges = Vec::new_in(BudgetAllocator::new(Arc::clone(&self.metadata)));
        ranges.try_reserve_exact(bytes / BLOCK_SIZE).map_err(|_| {
            io::Error::new(io::ErrorKind::OutOfMemory, "read plan metadata exhausted")
        })?;
        let mut covered = [0u64; 4];
        for (begin, mapping) in self.index.overlapping(offset, end) {
            let first = begin.max(offset);
            let last = mapping.end.min(end);
            for block in
                (first - offset) as usize / BLOCK_SIZE..(last - offset) as usize / BLOCK_SIZE
            {
                covered[block / 64] |= 1 << (block % 64);
            }
            let Some((payload, payload_offset)) = &mapping.source else {
                continue;
            };
            let skip = payload_offset + first - begin;
            debug_assert!(skip + last - first <= payload.bytes as u64);
            debug_assert_eq!(payload.sequence, mapping.sequence);
            payload.segment.pins.acquire(payload.batch_block);
            ranges.push(ReadRange {
                payload: payload.clone(),
                source: skip as usize..(skip + last - first) as usize,
                destination: (first - offset) as usize..(last - offset) as usize,
            });
        }
        // All mappings and requests are block aligned; no payload fragment is
        // smaller than a block, so a plan has at most 256 entries.
        debug_assert!(ranges.len() <= bytes / BLOCK_SIZE);
        Ok(ReadPlan {
            offset,
            bytes,
            ranges,
            covered,
            base: self.base.clone(),
        })
    }
}
