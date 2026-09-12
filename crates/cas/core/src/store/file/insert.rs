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
            state
                .segments
                .last()
                .is_none_or(|s| s.sealed || bytes > s.header.capacity - s.end)
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
        let publications = missing
            .iter()
            .map(|chunk| Publication {
                hash: chunk.hash(),
                previous: None,
            })
            .collect::<ArrayVec<_, MAX_CHUNKS>>();
        let (bytes, _) = self.append(builder, &publications, needs_segment)?;
        inserted.written = missing.len();
        inserted.encoded_bytes = bytes;
        Ok(inserted)
    }

    /// One durable output path for insertion and quiescent relocation.
    pub(super) fn append(
        &mut self,
        builder: Builder<BudgetAllocator>,
        publications: &[Publication],
        new_segment: bool,
    ) -> io::Result<(usize, Builder<BudgetAllocator>)> {
        require(
            !publications.is_empty() && publications.len() == builder.len(),
            "chunk publication count",
        )?;
        if new_segment {
            reserve(&mut self.shared.lock().segments, 1)?;
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
            require(
                !segment.sealed
                    && ((publications.len() + 1) * BLOCK_SIZE) as u64
                        <= segment.header.capacity - segment.end,
                "chunk destination capacity",
            )?;
            reserve(&mut segment.batches, 1)?;
            let next_batch = segment
                .next_batch
                .checked_add(1)
                .ok_or_else(|| io::Error::other("chunk batch IDs exhausted"))?;
            let next_ordinal = segment
                .next_ordinal
                .checked_add(publications.len() as u64)
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
        let addresses = publications
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
            for (publication, address) in publications.iter().zip(addresses) {
                match publication.previous {
                    Some(old) => state.index.relocate(&publication.hash, old, address)?,
                    None => {
                        state.index.insert_reserved(publication.hash, address);
                    }
                }
            }
            let segment = state.segments.last_mut().expect("insertion owns a segment");
            assert_eq!((segment.header.number, segment.end), (number, offset));
            segment.batches.push(BatchLocation {
                offset: offset as u32,
                chunks: publications.len() as u16,
            });
            segment.end += bytes;
            segment.file_bytes = segment.end;
            segment.next_batch = next_batch;
            segment.next_ordinal = next_ordinal;
            guard.pending = false;
        }
        Ok((bytes as usize, batch.into_builder()))
    }
}

pub(super) struct Publication {
    pub hash: Hash,
    pub previous: Option<Address>,
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
