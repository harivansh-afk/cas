use super::*;

mod sweep;

/// One original candidate; output capacity excludes the governor's metadata margin.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Victim {
    pub ticket: u64,
    pub live_chunks: usize,
    pub total_chunks: usize,
    pub destination_bytes: u64,
}

#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct Collected {
    pub segments_removed: usize,
    pub headers_retained: usize,
    pub chunks_copied: usize,
    pub encoded_bytes_copied: u64,
}

/// Requires the caller's complete, stable image/snapshot/old-root set.
pub struct Collection<'a> {
    store: &'a mut Store,
    guard: Guard,
    victims: Vec<Victim, BudgetAllocator>,
    header: AlignedBuffer<BudgetAllocator>,
    payload: AlignedBuffer<BudgetAllocator>,
    builder: Option<Builder<BudgetAllocator>>,
}

pub struct Sweep<'a> {
    collection: Collection<'a>,
    position: usize,
    totals: Collected,
}

impl Store {
    pub fn begin_collection(&mut self) -> io::Result<Collection<'_>> {
        let shared = Arc::clone(&self.shared);
        let mut state = shared.lock();
        state.healthy()?;
        require(
            !shared.tickets.status().failed,
            "chunk collection tickets failed",
        )?;
        if state.collecting
            || state
                .segments
                .iter()
                .any(|s| Arc::strong_count(&s.file) != 1)
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "chunk IO pins have not drained",
            ));
        }
        let mut victims = Vec::new_in(BudgetAllocator::new(Arc::clone(&shared.metadata)));
        victims
            .try_reserve_exact(state.segments.len())
            .map_err(|_| {
                io::Error::new(io::ErrorKind::OutOfMemory, "chunk collection candidates")
            })?;
        for segment in &state.segments {
            victims.push(Victim {
                ticket: segment.header.number,
                live_chunks: 0,
                total_chunks: segment.batches.iter().map(|b| usize::from(b.chunks)).sum(),
                destination_bytes: 0,
            });
        }
        let allocator = BudgetAllocator::new(Arc::clone(&self.io_memory));
        let header = AlignedBuffer::try_new_in(BLOCK_SIZE, allocator.clone())?;
        let payload = AlignedBuffer::try_new_in(MAX_CHUNKS * BLOCK_SIZE, allocator.clone())?;
        let builder = Builder::try_new_in(MAX_CHUNKS, allocator)?;
        state.index.clear_marks();
        state.collecting = true;
        drop(state);
        Ok(Collection {
            store: self,
            guard: Guard {
                shared,
                pending: true,
            },
            victims,
            header,
            payload,
            builder: Some(builder),
        })
    }
}

impl<'a> Collection<'a> {
    pub fn mark(&mut self, hash: &Hash) -> io::Result<()> {
        let result = (|| {
            let mut state = self.guard.shared.lock();
            state.healthy()?;
            state.index.mark(hash)
        })();
        self.guard.checked(result)
    }

    pub fn finish_marking(mut self) -> io::Result<Sweep<'a>> {
        let result = (|| {
            let state = self.guard.shared.lock();
            state.healthy()?;
            for (_, address, marked) in state.index.entries() {
                if marked {
                    let index = self
                        .victims
                        .binary_search_by_key(&address.segment(), |v| v.ticket)
                        .map_err(|_| io::Error::other("marked chunk has no segment"))?;
                    self.victims[index].live_chunks += 1;
                }
            }
            for victim in &mut self.victims {
                require(
                    victim.live_chunks <= victim.total_chunks,
                    "chunk collection live count",
                )?;
                if victim.live_chunks != 0 && victim.live_chunks < victim.total_chunks {
                    victim.destination_bytes = self.store.config.segment_bytes;
                }
            }
            Ok(())
        })();
        self.guard.checked(result)?;
        Ok(Sweep {
            collection: self,
            position: 0,
            totals: Collected::default(),
        })
    }
}

impl Sweep<'_> {
    pub fn next_victim(&self) -> Option<Victim> {
        self.collection.victims.get(self.position).copied()
    }

    /// Run one candidate under its physical promise. No victim file pin escapes.
    pub fn clean_next(&mut self) -> io::Result<Option<Collected>> {
        let Some(victim) = self.next_victim() else {
            return Ok(None);
        };
        let result = self.collection.clean(victim);
        let cleaned = self.collection.guard.checked(result)?;
        self.position += 1;
        self.totals.segments_removed += cleaned.segments_removed;
        self.totals.headers_retained += cleaned.headers_retained;
        self.totals.chunks_copied += cleaned.chunks_copied;
        self.totals.encoded_bytes_copied += cleaned.encoded_bytes_copied;
        Ok(Some(cleaned))
    }

    pub fn finish(mut self) -> io::Result<Collected> {
        require(
            self.position == self.collection.victims.len(),
            "chunk sweep unfinished",
        )?;
        let mut state = self.collection.guard.shared.lock();
        state.healthy()?;
        state.index.remove_unmarked();
        state.collecting = false;
        self.collection.guard.pending = false;
        Ok(self.totals)
    }
}

struct Guard {
    shared: Arc<Shared>,
    pending: bool,
}

impl Guard {
    fn checked<T>(&self, result: io::Result<T>) -> io::Result<T> {
        if result.is_err() {
            self.shared.lock().failed = true;
        }
        result
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.pending {
            self.shared.lock().failed = true;
        }
    }
}
