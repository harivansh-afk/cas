use super::{SnapshotKey, Tree};
use crate::{
    budget::{Budget, BudgetAllocator, BudgetArc},
    encoding::require,
    manifest::format::Extent,
};
use allocator_api2::vec::Vec;
use hashbrown::HashTable;
use std::{
    fs::File,
    io,
    sync::{Arc, Mutex},
};

struct Entry {
    key: SnapshotKey,
    owners: usize,
}

struct Registry(Mutex<HashTable<Entry, BudgetAllocator>>);

pub(super) struct Pin {
    registry: BudgetArc<Registry>,
    end: u64,
}

// COMMIT ends are unique within one actual file. Mix their aligned offsets so
// both the table bucket and its high-bit fingerprint vary across publications.
fn bucket(end: u64) -> u64 {
    (end / crate::BLOCK_SIZE as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

impl Pin {
    pub(super) fn new(key: SnapshotKey, metadata: Arc<Budget>) -> io::Result<Self> {
        key.validate()?;
        let registry = BudgetArc::try_new(
            Registry(Mutex::new(HashTable::new_in(BudgetAllocator::new(
                Arc::clone(&metadata),
            )))),
            &metadata,
        )?;
        Self::insert(registry, key)
    }

    pub(super) fn successor(&self, key: SnapshotKey) -> io::Result<Self> {
        key.validate()?;
        Self::insert(self.registry.clone(), key)
    }

    fn insert(registry: BudgetArc<Registry>, key: SnapshotKey) -> io::Result<Self> {
        {
            let mut entries = registry.0.lock().expect("root registry mutex poisoned");
            if let Some(entry) = entries.find_mut(bucket(key.end), |entry| entry.key.end == key.end)
            {
                require(
                    entry.key == key,
                    "different roots at one manifest COMMIT end",
                )?;
                entry.owners = entry
                    .owners
                    .checked_add(1)
                    .expect("root pin count exhausted");
            } else {
                entries
                    .try_reserve(1, |entry| bucket(entry.key.end))
                    .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
                entries.insert_unique(bucket(key.end), Entry { key, owners: 1 }, |entry| {
                    bucket(entry.key.end)
                });
            }
        }
        Ok(Self {
            registry,
            end: key.end,
        })
    }

    pub(super) fn capture<'a>(
        &self,
        file: &'a File,
        metadata: Arc<Budget>,
    ) -> io::Result<Roots<'a>> {
        let mut keys = Vec::new_in(BudgetAllocator::new(Arc::clone(&metadata)));
        {
            let entries = self
                .registry
                .0
                .lock()
                .expect("root registry mutex poisoned");
            keys.try_reserve_exact(entries.len())
                .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
            keys.extend(entries.iter().map(|entry| entry.key));
        }
        keys.sort_unstable_by_key(|key| key.end);
        Ok(Roots {
            file,
            keys,
            metadata,
        })
    }
}

impl Clone for Pin {
    fn clone(&self) -> Self {
        let mut entries = self
            .registry
            .0
            .lock()
            .expect("root registry mutex poisoned");
        let entry = entries
            .find_mut(bucket(self.end), |entry| entry.key.end == self.end)
            .expect("a live pin has a registered root");
        entry.owners = entry
            .owners
            .checked_add(1)
            .expect("root pin count exhausted");
        Self {
            registry: self.registry.clone(),
            end: self.end,
        }
    }
}

impl Drop for Pin {
    fn drop(&mut self) {
        let mut entries = self
            .registry
            .0
            .lock()
            .expect("root registry mutex poisoned");
        let mut entry = entries
            .find_entry(bucket(self.end), |entry| entry.key.end == self.end)
            .ok()
            .expect("a live pin has a registered root");
        entry.get_mut().owners -= 1;
        if entry.get().owners == 0 {
            entry.remove();
        }
    }
}

/// Captures all registered roots under an exclusive owner borrow. A dropped
/// reader can only make this set conservative; another publication is excluded.
pub struct Roots<'a> {
    pub(super) file: &'a File,
    pub(super) keys: Vec<SnapshotKey, BudgetAllocator>,
    pub(super) metadata: Arc<Budget>,
}

impl Roots<'_> {
    pub fn keys(&self) -> &[SnapshotKey] {
        &self.keys
    }

    /// Validate and visit every reachable mapping, including old captured roots.
    /// Quiescent GC can mark chunk IDs without allocating a second tree/index.
    pub fn walk(&self, mut visitor: impl FnMut(Extent) -> io::Result<()>) -> io::Result<()> {
        for key in &self.keys {
            Tree::new(self.file, key.commit, key.end, Arc::clone(&self.metadata))?
                .walk(&mut visitor)?;
        }
        Ok(())
    }
}
