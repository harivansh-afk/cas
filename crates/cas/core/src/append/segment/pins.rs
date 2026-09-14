//! Fixed counters allocated before segment creation, reused by every read.
use crate::{
    BLOCK_SIZE,
    budget::{Budget, BudgetAllocator},
};
use allocator_api2::vec::Vec;
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

#[derive(Debug)]
pub(in crate::append) struct Pins {
    batches: Vec<AtomicU32, BudgetAllocator>,
    readers: AtomicU32,
}

impl Pins {
    pub fn new(capacity: u64, metadata: Arc<Budget>) -> io::Result<Self> {
        let blocks = u32::try_from(capacity / BLOCK_SIZE as u64)
            .map_err(|_| io::Error::other("staging pin table exceeds address bound"))?;
        let mut batches = Vec::new_in(BudgetAllocator::new(metadata));
        batches.try_reserve_exact(blocks as usize).map_err(|_| {
            io::Error::new(io::ErrorKind::OutOfMemory, "staging pin metadata exhausted")
        })?;
        batches.resize_with(blocks as usize, || AtomicU32::new(0));
        Ok(Self {
            batches,
            readers: AtomicU32::new(0),
        })
    }
    pub fn bytes(&self) -> usize {
        self.batches.capacity() * size_of::<AtomicU32>()
    }
    pub fn readers(&self) -> u32 {
        self.readers.load(Ordering::Acquire)
    }
    pub fn at(&self, block: u32) -> u32 {
        self.batches[block as usize].load(Ordering::Acquire)
    }

    /// Read capture is sequenced against removal of the corresponding mapping.
    pub fn acquire(&self, block: u32) {
        let old = self.batches[block as usize].fetch_add(1, Ordering::Relaxed);
        assert!(old < u32::MAX, "bounded read ownership overflow");
        let old = self.readers.fetch_add(1, Ordering::Relaxed);
        assert!(old < u32::MAX, "bounded segment ownership overflow");
    }
    pub fn release(&self, block: u32) {
        assert!(self.batches[block as usize].fetch_sub(1, Ordering::Release) != 0);
        assert!(self.readers.fetch_sub(1, Ordering::Release) != 0);
    }

    /// A background scan holds the whole segment through the header block,
    /// which no batch payload ever occupies.
    pub fn hold_scan(&self) {
        self.acquire(0);
    }
    pub fn release_scan(&self) {
        self.release(0);
    }
    pub fn scan_held(&self) -> bool {
        self.at(0) != 0
    }
}
