use super::*;
use arrayvec::ArrayVec;

impl Store {
    pub fn insert(&mut self, chunks: &[Chunk<'_>]) -> io::Result<Inserted> {
        require(
            chunks.len() <= MAX_CHUNKS,
            "chunk insertion exceeds batch bound",
        )?;
        let mut missing = ArrayVec::<Chunk<'_>, MAX_CHUNKS>::new();
        let needs_segment = {
            let mut state = self.shared.lock();
            state.healthy()?;
            for &chunk in chunks {
                if state.index.get(&chunk.hash()).is_none()
                    && !missing.iter().any(|old| old.hash() == chunk.hash())
                {
                    missing.push(chunk);
                }
            }
            state.index.reserve(missing.len())?;
            let bytes = ((missing.len() + 1) * BLOCK_SIZE) as u64;
            let needs_segment = state
                .segments
                .last()
                .is_none_or(|s| bytes > s.header.capacity - s.end);
            if needs_segment && !missing.is_empty() {
                reserve(&mut state.segments, 1)?;
            }
            needs_segment
        };
        let mut inserted = Inserted {
            reused: chunks.len() - missing.len(),
            ..Inserted::default()
        };
        if missing.is_empty() {
            return Ok(inserted);
        }
        let mut builder = Builder::try_new_in(
            missing.len(),
            BudgetAllocator::new(Arc::clone(&self.io_memory)),
        )?;
        for &chunk in &missing {
            builder.push(chunk)?;
        }
        if needs_segment {
            // This reserves the batch table and scratch before namespace IO.
            // Ticket failure includes an interrupted header creation.
            let mut guard = Output {
                shared: &self.shared,
                pending: true,
            };
            match Segment::create(
                &self.shared.directory,
                &self.shared.tickets,
                self.config,
                Arc::clone(&self.shared.metadata),
            ) {
                Ok(segment) => self.shared.lock().segments.push(segment),
                Err(error) => {
                    guard.pending = self.shared.tickets.status().failed;
                    return Err(error);
                }
            }
            guard.pending = false;
        }
        let (file, offset, number, batch_id, ordinal, next_batch, next_ordinal) = {
            let mut state = self.shared.lock();
            state.healthy()?;
            let segment = state.segments.last_mut().expect("insertion owns a segment");
            reserve(&mut segment.batches, 1)?;
            let next_batch = segment
                .next_batch
                .checked_add(1)
                .ok_or_else(|| io::Error::other("chunk batch IDs exhausted"))?;
            let next_ordinal = segment
                .next_ordinal
                .checked_add(missing.len() as u64)
                .ok_or_else(|| io::Error::other("chunk ordinals exhausted"))?;
            (
                Arc::clone(&segment.file),
                segment.end,
                segment.header.number,
                segment.next_batch,
                segment.next_ordinal,
                next_batch,
                next_ordinal,
            )
        };
        let batch = builder.seal(number, batch_id, ordinal)?;
        let addresses = missing
            .iter()
            .enumerate()
            .map(|(index, _)| Address::new(number, offset + ((index + 1) * BLOCK_SIZE) as u64))
            .collect::<io::Result<ArrayVec<_, MAX_CHUNKS>>>()?;
        let bytes = batch.bytes().len() as u64;
        let mut guard = Output {
            shared: &self.shared,
            pending: true,
        };
        // Recovery may have truncated the segment's original preallocation.
        // No lookup lock spans allocation, payload IO or sync.
        direct::preallocate(&file, offset, bytes)?;
        direct::write_bytes(&file, batch.bytes(), offset)?;
        direct::sync_data(&file)?;
        {
            let mut state = self.shared.lock();
            state.healthy()?;
            for (chunk, address) in missing.iter().zip(addresses) {
                state.index.insert_reserved(chunk.hash(), address);
            }
            let segment = state.segments.last_mut().expect("insertion owns a segment");
            assert_eq!((segment.header.number, segment.end), (number, offset));
            segment.batches.push(BatchLocation {
                offset: offset as u32,
                chunks: missing.len() as u16,
            });
            segment.end += bytes;
            segment.file_bytes = segment.end;
            segment.next_batch = next_batch;
            segment.next_ordinal = next_ordinal;
            guard.pending = false;
        }
        inserted.written = missing.len();
        inserted.encoded_bytes = bytes as usize;
        Ok(inserted)
    }
}

/// Publish terminal failure before returning an output error, including unwind.
/// An in-progress write alone must not make already committed chunks unreadable.
struct Output<'a> {
    shared: &'a Shared,
    pending: bool,
}

impl Drop for Output<'_> {
    fn drop(&mut self) {
        if self.pending {
            self.shared.lock().failed = true;
        }
    }
}
