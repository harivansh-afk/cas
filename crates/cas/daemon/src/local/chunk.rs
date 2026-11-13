//! One per-block resolution for both read schedulers: the reactor's staged
//! read (`reactor/read.rs`) and the frontend-inline settled read (`fast.rs`).
//! Holes and clean-cache hits are served here; everything else is handed back
//! with what the scheduler needs: a manifest page to load, a verified local
//! store plan, or a chunk only a peer can serve. The cache-fill rules live
//! here too, so neither scheduler can drift from the other.
use super::*;
use cas_core::{
    cache::Cache,
    chunk_index::Hash,
    manifest::{
        file::{self as manifest, PageCache, View},
        tree::LookupState,
    },
    store::file::{self as store, Reader},
};

pub(super) struct Sources {
    reader: Reader,
    cache: BudgetArc<Cache>,
    pages: BudgetArc<PageCache>,
    force_remote: bool,
    cache_remote: bool,
}

/// One block of a read after resolution against the manifest and caches.
pub(super) enum Block {
    /// Zeros were written to the destination.
    Hole,
    /// Clean-cache bytes were copied to the destination.
    Cached,
    /// A manifest page must be loaded before this block resolves.
    Page { read: manifest::Read, offset: u64 },
    /// A chunk with a verified local store plan.
    Store(Hash, store::Read),
    /// A chunk only a peer can serve, or force-remote demands a peer.
    Remote(Hash),
}

/// Where a known chunk hash is served from.
pub(super) enum Located {
    Cached,
    Store(store::Read),
    Remote,
}

impl Sources {
    pub fn new(port: &host::Port) -> Self {
        Self {
            reader: port.reader(),
            cache: port.cache(),
            pages: port.page_cache(),
            force_remote: port.peer_config().peer_force_remote,
            cache_remote: port.peer_config().peer_cache,
        }
    }
    pub fn pages(&self) -> &PageCache {
        &self.pages
    }
    /// Force-remote bypasses the clean cache too unless fills are enabled.
    fn serve_cache(&self) -> bool {
        !self.force_remote || self.cache_remote
    }

    /// Resolve one absolute block into `destination` (one guest block).
    pub fn resolve(&self, view: &View, block: u64, destination: &mut [u8]) -> io::Result<Block> {
        let mut lookup = view.lookup(block)?;
        match lookup.cached(&self.pages)? {
            LookupState::Page { offset, .. } => Ok(Block::Page {
                read: lookup,
                offset,
            }),
            LookupState::Complete(None) => {
                destination.fill(0);
                Ok(Block::Hole)
            }
            LookupState::Complete(Some(hash)) => {
                Ok(match self.locate(view, block, hash, destination)? {
                    Located::Cached => Block::Cached,
                    Located::Store(plan) => Block::Store(hash, plan),
                    Located::Remote => Block::Remote(hash),
                })
            }
        }
    }

    /// Serve a known hash from the clean cache, else say where it lives.
    pub fn locate(
        &self,
        view: &View,
        block: u64,
        hash: Hash,
        destination: &mut [u8],
    ) -> io::Result<Located> {
        if self.serve_cache()
            && let Some(bytes) = self.cache.get(&hash)
        {
            copy_chunk(view, block, bytes.as_slice(), destination)?;
            return Ok(Located::Cached);
        }
        if self.force_remote {
            return Ok(Located::Remote);
        }
        Ok(match self.reader.plan(hash)? {
            Some(plan) => Located::Store(plan),
            None => Located::Remote,
        })
    }

    /// A chunk another leader may have filled between a miss and the fetch
    /// registry lookup; `peek` does not count as a hit.
    pub fn peek(&self, hash: &Hash) -> Option<cas_core::cache::Buffer> {
        if self.serve_cache() {
            self.cache.peek(hash)
        } else {
            None
        }
    }

    /// The verified local plan for a race against a forced remote fetch.
    pub fn race_plan(&self, hash: Hash) -> Option<store::Read> {
        if self.force_remote {
            self.reader.plan(hash).ok().flatten()
        } else {
            None
        }
    }

    /// Fill rule: verified local store payload always; a verified remote
    /// reply only with `--peer-cache`. A full cache is not an error.
    pub fn fill(&self, hash: Hash, bytes: &[u8], remote: bool) -> io::Result<()> {
        if remote && !self.cache_remote {
            return Ok(());
        }
        match self.cache.fill(hash, bytes) {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::OutOfMemory => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// Copy the guest block `block` out of a whole chunk.
pub(super) fn copy_chunk(
    view: &View,
    block: u64,
    chunk: &[u8],
    destination: &mut [u8],
) -> io::Result<()> {
    if chunk.len() != view.chunk_size().bytes() || destination.len() != BLOCK_SIZE {
        return Err(io::Error::other("fetched chunk geometry differs"));
    }
    let offset = view.chunk_size().offset(block);
    destination.copy_from_slice(&chunk[offset..offset + BLOCK_SIZE]);
    Ok(())
}
