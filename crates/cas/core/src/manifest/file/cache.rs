//! Verified immutable pages; cache membership never pins a manifest root.
use crate::{
    BLOCK_SIZE,
    budget::{Amount, Budget, BudgetArc, Lease},
    cache::{Key, Lru, Status},
};
use std::{
    io,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct PageKey {
    pub incarnation: u64,
    pub end: u64,
    pub offset: u64,
}

impl Key for PageKey {
    fn bucket(&self) -> u64 {
        self.incarnation.wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ self.end.rotate_left(21)
            ^ self.offset.rotate_left(43)
    }
}

pub(super) struct CachedPage {
    pub bytes: [u8; BLOCK_SIZE],
    _credit: Lease,
}

pub struct PageCache {
    state: Mutex<Lru<PageKey, CachedPage>>,
    capacity_bytes: usize,
    pages: Arc<Budget>,
    metadata: Arc<Budget>,
}

impl PageCache {
    pub fn new(bytes: usize, metadata: &Arc<Budget>) -> io::Result<BudgetArc<Self>> {
        if bytes == 0 || !bytes.is_multiple_of(BLOCK_SIZE) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid page-cache byte limit",
            ));
        }
        BudgetArc::try_new(
            Self {
                state: Mutex::new(Lru::new(bytes / BLOCK_SIZE, metadata)?),
                capacity_bytes: bytes,
                pages: Budget::new(Amount { bytes, requests: 0 }),
                metadata: Arc::clone(metadata),
            },
            metadata,
        )
    }

    pub(super) fn get(&self, key: &PageKey) -> Option<BudgetArc<CachedPage>> {
        self.state.lock().expect("page cache poisoned").get(key)
    }

    /// Only the checked manifest descent may publish a page.
    pub(super) fn fill(&self, key: PageKey, bytes: &[u8]) -> io::Result<()> {
        let mut state = self.state.lock().expect("page cache poisoned");
        if state.buffer(&key).is_some() {
            state.promote(key);
            return Ok(());
        }
        let credit = loop {
            if let Some(credit) = self.pages.reserve(Amount {
                bytes: BLOCK_SIZE,
                requests: 0,
            }) {
                break credit;
            }
            if !state.evict() {
                state.counters.refused += 1;
                return Ok(());
            }
        };
        let page = BudgetArc::try_new(
            CachedPage {
                bytes: bytes.try_into().map_err(|_| io::ErrorKind::InvalidInput)?,
                _credit: credit,
            },
            &self.metadata,
        )
        .inspect_err(|_| {
            state.counters.refused += 1;
        })?;
        state.insert(key, page);
        Ok(())
    }

    /// Payload usage is included in foreground metadata, not an extra allocation.
    pub fn status(&self) -> Status {
        let state = self.state.lock().expect("page cache poisoned");
        let payload = self.pages.usage();
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
