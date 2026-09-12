//! Fixed-capacity membership shared by verified chunk and manifest-page caches.
use super::Counters;
use crate::budget::{Budget, BudgetAllocator, BudgetArc};
use hashbrown::HashTable;
use std::{io, sync::Arc};

pub(crate) trait Key: Copy + Eq {
    fn bucket(&self) -> u64;
}

struct Entry<K, V> {
    key: K,
    buffer: BudgetArc<V>,
    older: Option<K>,
    newer: Option<K>,
}

pub(crate) struct Lru<K, V> {
    entries: HashTable<Entry<K, V>, BudgetAllocator>,
    capacity: usize,
    oldest: Option<K>,
    newest: Option<K>,
    pub counters: Counters,
}

impl<K: Key, V> Lru<K, V> {
    pub fn new(capacity: usize, metadata: &Arc<Budget>) -> io::Result<Self> {
        let mut entries = HashTable::new_in(BudgetAllocator::new(Arc::clone(metadata)));
        // Pinned hashbrown rehashes tombstones in place below half capacity.
        // Reserve that headroom once so eviction churn never grows the table.
        let slots = capacity.checked_mul(2).ok_or(io::ErrorKind::OutOfMemory)?;
        entries
            .try_reserve(slots, |entry: &Entry<K, V>| entry.key.bucket())
            .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
        Ok(Self {
            entries,
            capacity,
            oldest: None,
            newest: None,
            counters: Counters::default(),
        })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn table_bytes(&self) -> usize {
        self.entries.allocation_size()
    }

    pub fn buffer(&self, key: &K) -> Option<BudgetArc<V>> {
        self.entries
            .find(key.bucket(), |entry| &entry.key == key)
            .map(|entry| entry.buffer.clone())
    }

    pub fn get(&mut self, key: &K) -> Option<BudgetArc<V>> {
        let Some(buffer) = self.buffer(key) else {
            self.counters.misses += 1;
            return None;
        };
        self.promote(*key);
        self.counters.hits += 1;
        Some(buffer)
    }

    pub fn insert(&mut self, key: K, buffer: BudgetArc<V>) {
        // Payload admission bounds residents, independently of tombstones.
        assert!(self.entries.len() < self.capacity);
        self.entries.insert_unique(
            key.bucket(),
            Entry {
                key,
                buffer,
                older: None,
                newer: None,
            },
            |entry| entry.key.bucket(),
        );
        self.link_newest(key);
        self.counters.fills += 1;
    }

    fn entry(&mut self, key: K) -> &mut Entry<K, V> {
        self.entries
            .find_mut(key.bucket(), |entry| entry.key == key)
            .expect("LRU link names a resident entry")
    }

    fn unlink(&mut self, key: K) {
        let entry = self.entry(key);
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

    pub fn promote(&mut self, key: K) {
        if self.newest == Some(key) {
            return;
        }
        self.unlink(key);
        self.link_newest(key);
    }

    fn link_newest(&mut self, key: K) {
        let newest = self.newest;
        let entry = self.entry(key);
        entry.older = newest;
        entry.newer = None;
        if let Some(newest) = newest {
            self.entry(newest).newer = Some(key);
        } else {
            self.oldest = Some(key);
        }
        self.newest = Some(key);
    }

    pub fn evict(&mut self) -> bool {
        let Some(key) = self.oldest else {
            return false;
        };
        self.unlink(key);
        let entry = self
            .entries
            .find_entry(key.bucket(), |entry| entry.key == key)
            .unwrap_or_else(|_| panic!("oldest cache entry exists"))
            .remove()
            .0;
        drop(entry);
        self.counters.evictions += 1;
        true
    }
}
