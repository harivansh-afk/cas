//! Verified clean blocks. LRU membership never owns a reader's last byte credit.
use crate::{
    BLOCK_SIZE,
    aligned::AlignedBuffer,
    budget::{Amount, Budget, BudgetAllocator, BudgetArc, Usage},
    chunk_index::Hash,
};
use std::{
    io,
    sync::{Arc, Mutex},
};

pub type Buffer = BudgetArc<AlignedBuffer<BudgetAllocator>>;

#[derive(Default, Clone, Copy, serde::Serialize)]
pub struct Counters {
    pub hits: u64,
    pub misses: u64,
    pub fills: u64,
    pub evictions: u64,
    pub refused: u64,
}

#[derive(serde::Serialize)]
pub struct Status {
    pub capacity_bytes: usize,
    pub counters: Counters,
    pub resident_bytes: usize,
    pub reader_held_bytes: usize,
    pub payload: Usage,
    pub table_bytes: usize,
}

mod lru;
pub(crate) use lru::{Key, Lru};

type State = Lru<Hash, AlignedBuffer<BudgetAllocator>>;

pub struct Cache {
    capacity_bytes: usize,
    state: Mutex<State>,
    payload: Arc<Budget>,
    metadata: Arc<Budget>,
}

impl Key for Hash {
    fn bucket(&self) -> u64 {
        crate::chunk_index::bucket(self)
    }
}

impl Cache {
    pub fn new(bytes: usize, metadata: &Arc<Budget>) -> io::Result<BudgetArc<Self>> {
        if bytes == 0 || !bytes.is_multiple_of(BLOCK_SIZE) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid cache byte limit",
            ));
        }
        BudgetArc::try_new(
            Self {
                capacity_bytes: bytes,
                state: Mutex::new(State::new(bytes / BLOCK_SIZE, metadata)?),
                payload: Budget::new(Amount { bytes, requests: 0 }),
                metadata: Arc::clone(metadata),
            },
            metadata,
        )
    }

    pub fn get(&self, hash: &Hash) -> Option<Buffer> {
        let wait = crate::io_metrics::measure(0, |c| &mut c.chunk_cache_wait);
        let mut state = self.state.lock().expect("cache poisoned");
        drop(wait);
        let _hold = crate::io_metrics::measure(0, |c| &mut c.chunk_cache_hold);
        state.get(hash)
    }

    /// Recheck after claiming a fetch without counting a second guest lookup.
    pub fn peek(&self, hash: &Hash) -> Option<Buffer> {
        let wait = crate::io_metrics::measure(0, |c| &mut c.chunk_cache_wait);
        let state = self.state.lock().expect("cache poisoned");
        drop(wait);
        let _hold = crate::io_metrics::measure(0, |c| &mut c.chunk_cache_hold);
        state.buffer(hash)
    }

    /// Read-fill publication. None means reader-held bytes prevent a new fill.
    pub fn fill(&self, hash: Hash, bytes: &[u8]) -> io::Result<Option<Buffer>> {
        if bytes.len() != BLOCK_SIZE || blake3::hash(bytes).as_bytes() != &hash {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid cache fill",
            ));
        }
        let wait = crate::io_metrics::measure(0, |c| &mut c.chunk_cache_wait);
        let mut state = self.state.lock().expect("cache poisoned");
        drop(wait);
        let _hold = crate::io_metrics::measure(0, |c| &mut c.chunk_cache_hold);
        if let Some(buffer) = state.buffer(&hash) {
            state.promote(hash);
            return Ok(Some(buffer));
        }
        let mut buffer = loop {
            match AlignedBuffer::try_new_in(
                BLOCK_SIZE,
                BudgetAllocator::new(Arc::clone(&self.payload)),
            ) {
                Ok(buffer) => break buffer,
                Err(error) if error.kind() == io::ErrorKind::OutOfMemory => {
                    if !state.evict() {
                        state.counters.refused += 1;
                        return Ok(None);
                    }
                }
                Err(error) => return Err(error),
            }
        };
        buffer.as_mut_slice().copy_from_slice(bytes);
        let buffer = BudgetArc::try_new(buffer, &self.metadata).inspect_err(|_| {
            state.counters.refused += 1;
        })?;
        state.insert(hash, buffer.clone());
        Ok(Some(buffer))
    }

    pub fn clear(&self) {
        let wait = crate::io_metrics::measure(0, |c| &mut c.chunk_cache_wait);
        let mut state = self.state.lock().expect("cache poisoned");
        drop(wait);
        let _hold = crate::io_metrics::measure(0, |c| &mut c.chunk_cache_hold);
        while state.evict() {}
    }

    pub fn status(&self) -> Status {
        let wait = crate::io_metrics::measure(0, |c| &mut c.chunk_cache_wait);
        let state = self.state.lock().expect("cache poisoned");
        drop(wait);
        let _hold = crate::io_metrics::measure(0, |c| &mut c.chunk_cache_hold);
        let payload = self.payload.usage();
        let resident_bytes = state.len() * BLOCK_SIZE;
        Status {
            capacity_bytes: self.capacity_bytes,
            counters: state.counters,
            resident_bytes,
            reader_held_bytes: payload.current.bytes - resident_bytes,
            payload,
            table_bytes: state.table_bytes(),
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(target_os = "linux")]
pub mod fills;
