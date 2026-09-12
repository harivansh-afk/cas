//! Rebuildable hash-to-location index. Store publication requires durable data.
use std::{io, sync::Arc};

use hashbrown::HashTable;

use crate::{
    BLOCK_SIZE,
    budget::{Budget, BudgetAllocator},
};

pub type Hash = [u8; 32];
pub const MAX_SEGMENT: u64 = (1 << 48) - 1;
pub const MAX_SEGMENT_BYTES: u64 = (1 << 16) * BLOCK_SIZE as u64;

/// A checked 48-bit segment ticket and 16-bit payload block index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address(u64);

impl Address {
    pub fn new(segment: u64, offset: u64) -> io::Result<Self> {
        if segment == 0
            || segment > MAX_SEGMENT
            || offset == 0
            || offset >= MAX_SEGMENT_BYTES
            || !offset.is_multiple_of(BLOCK_SIZE as u64)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "chunk address out of range",
            ));
        }
        Ok(Self((segment << 16) | (offset / BLOCK_SIZE as u64)))
    }

    pub fn segment(self) -> u64 {
        self.0 >> 16
    }
    pub fn offset(self) -> u64 {
        (self.0 & 0xffff) * BLOCK_SIZE as u64
    }
}

#[derive(Clone, Copy)]
struct Entry {
    hash: Hash,
    address: Address,
    marked: bool,
}

pub struct Index {
    table: HashTable<Entry, BudgetAllocator>,
}

fn bucket(hash: &Hash) -> u64 {
    u64::from_le_bytes(hash[..8].try_into().unwrap())
}

impl Index {
    pub fn new(metadata: Arc<Budget>) -> Self {
        Self {
            table: HashTable::new_in(BudgetAllocator::new(metadata)),
        }
    }

    pub fn len(&self) -> usize {
        self.table.len()
    }
    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }
    pub fn allocated_bytes(&self) -> usize {
        self.table.allocation_size()
    }

    /// Reserve before writing corresponding new store data. The allocator also
    /// charges the old table while the replacement is being allocated.
    pub fn reserve(&mut self, additional: usize) -> io::Result<()> {
        self.table
            .try_reserve(additional, |entry| bucket(&entry.hash))
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "chunk index metadata budget exhausted",
                )
            })
    }

    pub fn get(&self, hash: &Hash) -> Option<Address> {
        self.table
            .find(bucket(hash), |entry| &entry.hash == hash)
            .map(|entry| entry.address)
    }

    /// The owner must verify and sync a new location before publishing it here.
    /// Duplicate publication keeps the existing address. Copying GC uses relocate.
    pub fn insert(&mut self, hash: Hash, address: Address) -> io::Result<Address> {
        if let Some(existing) = self.get(&hash) {
            return Ok(existing);
        }
        self.reserve(1)?;
        Ok(self.insert_reserved(hash, address))
    }

    /// Publication after output sync must use capacity reserved before that IO.
    pub(crate) fn insert_reserved(&mut self, hash: Hash, address: Address) -> Address {
        if let Some(existing) = self.get(&hash) {
            return existing;
        }
        assert!(
            self.table.len() < self.table.capacity(),
            "unreserved chunk publication"
        );
        self.table.insert_unique(
            bucket(&hash),
            Entry {
                hash,
                address,
                marked: false,
            },
            |entry| bucket(&entry.hash),
        );
        address
    }

    /// Replace a verified durable location under the quiescent GC owner.
    pub fn relocate(&mut self, hash: &Hash, old: Address, new: Address) -> io::Result<()> {
        let entry = self
            .table
            .find_mut(bucket(hash), |entry| &entry.hash == hash)
            .filter(|entry| entry.address == old)
            .ok_or_else(|| io::Error::other("chunk location changed during relocation"))?;
        entry.address = new;
        Ok(())
    }

    pub fn clear_marks(&mut self) {
        for entry in self.table.iter_mut() {
            entry.marked = false;
        }
    }

    pub fn mark(&mut self, hash: &Hash) -> io::Result<()> {
        let entry = self
            .table
            .find_mut(bucket(hash), |entry| &entry.hash == hash)
            .ok_or_else(|| io::Error::other("manifest references a missing chunk"))?;
        entry.marked = true;
        Ok(())
    }

    pub fn entries(&self) -> impl Iterator<Item = (&Hash, Address, bool)> {
        self.table
            .iter()
            .map(|entry| (&entry.hash, entry.address, entry.marked))
    }

    /// Only after the owning GC transaction has made its deletions durable.
    pub fn remove_unmarked(&mut self) {
        self.table.retain(|entry| entry.marked);
    }
}

#[cfg(test)]
mod tests;
