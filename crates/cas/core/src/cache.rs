//! Verified clean blocks. LRU membership never owns a reader's last byte credit.
use crate::{
    BLOCK_SIZE,
    aligned::AlignedBuffer,
    budget::{Amount, Budget, BudgetAllocator, BudgetArc, Usage},
    chunk_index::Hash,
};
use hashbrown::HashTable;
use std::{
    io,
    sync::{Arc, Mutex},
};

pub type Buffer = BudgetArc<AlignedBuffer<BudgetAllocator>>;

struct Entry {
    hash: Hash,
    buffer: Buffer,
    older: Option<Hash>,
    newer: Option<Hash>,
}

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

struct State {
    entries: HashTable<Entry, BudgetAllocator>,
    oldest: Option<Hash>,
    newest: Option<Hash>,
    counters: Counters,
}

pub struct Cache {
    capacity_bytes: usize,
    state: Mutex<State>,
    payload: Arc<Budget>,
    metadata: Arc<Budget>,
}

fn bucket(hash: &Hash) -> u64 {
    u64::from_le_bytes(hash[..8].try_into().unwrap())
}

impl Cache {
    pub fn new(bytes: usize, metadata: &Arc<Budget>) -> io::Result<BudgetArc<Self>> {
        if bytes == 0 || !bytes.is_multiple_of(BLOCK_SIZE) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid cache byte limit",
            ));
        }
        let mut entries = HashTable::new_in(BudgetAllocator::new(Arc::clone(metadata)));
        entries
            .try_reserve(bytes / BLOCK_SIZE, |entry: &Entry| bucket(&entry.hash))
            .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
        BudgetArc::try_new(
            Self {
                capacity_bytes: bytes,
                state: Mutex::new(State {
                    entries,
                    oldest: None,
                    newest: None,
                    counters: Counters::default(),
                }),
                payload: Budget::new(Amount { bytes, requests: 0 }),
                metadata: Arc::clone(metadata),
            },
            metadata,
        )
    }

    pub fn get(&self, hash: &Hash) -> Option<Buffer> {
        let mut state = self.state.lock().expect("cache poisoned");
        let Some(entry) = state
            .entries
            .find(bucket(hash), |entry| &entry.hash == hash)
        else {
            state.counters.misses += 1;
            return None;
        };
        let buffer = entry.buffer.clone();
        state.promote(*hash);
        state.counters.hits += 1;
        Some(buffer)
    }

    /// Read-fill publication. None means reader-held bytes prevent a new fill.
    pub fn fill(&self, hash: Hash, bytes: &[u8]) -> io::Result<Option<Buffer>> {
        if bytes.len() != BLOCK_SIZE || blake3::hash(bytes).as_bytes() != &hash {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid cache fill",
            ));
        }
        let mut state = self.state.lock().expect("cache poisoned");
        if let Some(entry) = state
            .entries
            .find(bucket(&hash), |entry| entry.hash == hash)
        {
            let buffer = entry.buffer.clone();
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
        // Payload admission implies a free preallocated entry: resident entries
        // each own a block from the same byte account, including evicted readers.
        assert!(state.entries.len() < state.entries.capacity());
        state.entries.insert_unique(
            bucket(&hash),
            Entry {
                hash,
                buffer: buffer.clone(),
                older: None,
                newer: None,
            },
            |entry| bucket(&entry.hash),
        );
        state.link_newest(hash);
        state.counters.fills += 1;
        Ok(Some(buffer))
    }

    pub fn clear(&self) {
        let mut state = self.state.lock().expect("cache poisoned");
        while state.evict() {}
    }

    pub fn status(&self) -> Status {
        let state = self.state.lock().expect("cache poisoned");
        let payload = self.payload.usage();
        let resident_bytes = state.entries.len() * BLOCK_SIZE;
        Status {
            capacity_bytes: self.capacity_bytes,
            counters: state.counters,
            resident_bytes,
            reader_held_bytes: payload.current.bytes - resident_bytes,
            payload,
            table_bytes: state.entries.allocation_size(),
        }
    }
}

impl State {
    fn entry(&mut self, hash: Hash) -> &mut Entry {
        self.entries
            .find_mut(bucket(&hash), |entry| entry.hash == hash)
            .expect("LRU link names a resident entry")
    }

    fn unlink(&mut self, hash: Hash) {
        let entry = self.entry(hash);
        let (older, newer) = (entry.older, entry.newer);
        if let Some(older) = older {
            self.entry(older).newer = newer;
        } else {
            self.oldest = newer;
        }
        if let Some(newer) = newer {
            self.entry(newer).older = older;
        } else {
            self.newest = older;
        }
    }

    fn promote(&mut self, hash: Hash) {
        if self.newest == Some(hash) {
            return;
        }
        self.unlink(hash);
        self.link_newest(hash);
    }

    fn link_newest(&mut self, hash: Hash) {
        let newest = self.newest;
        let entry = self.entry(hash);
        entry.older = newest;
        entry.newer = None;
        if let Some(newest) = newest {
            self.entry(newest).newer = Some(hash);
        } else {
            self.oldest = Some(hash);
        }
        self.newest = Some(hash);
    }

    fn evict(&mut self) -> bool {
        let Some(hash) = self.oldest else {
            return false;
        };
        self.unlink(hash);
        let entry = self
            .entries
            .find_entry(bucket(&hash), |entry| entry.hash == hash)
            .unwrap_or_else(|_| panic!("oldest cache entry exists"))
            .remove()
            .0;
        drop(entry);
        self.counters.evictions += 1;
        true
    }
}

#[cfg(test)]
mod tests;
