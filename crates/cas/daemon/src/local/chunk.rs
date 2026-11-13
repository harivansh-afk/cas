//! One per-block resolution for both read schedulers: the reactor's staged
//! read (`reactor/read.rs`) and the frontend-inline settled read (`fast.rs`).
//! Holes and clean-cache hits are served here; everything else is handed back
//! with what the scheduler needs: a manifest page to load, a verified local
//! store plan, or a chunk only a peer can serve. The cache-fill rules live
//! here too, so neither scheduler can drift from the other, and so does the
//! fetch-registry state machine: joining as leader or waiter, republishing a
//! fill that landed between the miss and the lookup, and publishing a
//! verified payload (fill rule, guest-block copy, waiter completion).
use super::*;
use cas_core::{
    aligned::AlignedBuffer,
    cache::{
        Cache,
        fills::{Leader, Lookup, Registry, Waiter},
    },
    chunk_index::Hash,
    manifest::{
        file::{self as manifest, PageCache, View},
        tree::LookupState,
    },
    store::file::{self as store, Reader},
};
use std::os::fd::OwnedFd;

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

/// This reader's position for one chunk after joining the fetch registry.
pub(super) enum Joined {
    /// Another reader leads; its publication signals the waiter (through the
    /// caller's wake when one was given).
    Wait(Waiter<Fetched>),
    /// This reader leads: it fetches the payload and must `Fetch::publish`
    /// it; dropping the `Fetch` fails every waiter.
    Lead(Fetch),
    /// A previous leader's cache fill arrived between the miss and the
    /// lookup; it was republished through a fresh cell and the destination
    /// block is filled.
    Served,
    /// The registry has no entry left.
    Full,
}

/// A led fetch: the registry entry and the payload cell waiters receive.
pub(super) struct Fetch {
    leader: Leader<Fetched>,
    cell: BudgetArc<Fetched>,
}

impl Fetch {
    /// The payload buffer a store read or a GET reply fills.
    pub fn payload_mut(&mut self) -> &mut [u8] {
        self.cell
            .get_mut()
            .expect("unpublished fetch is unique")
            .bytes
            .as_mut_slice()
    }
    pub fn payload(&self) -> &[u8] {
        self.cell.bytes.as_slice()
    }
    /// Publish a verified payload: the cache-fill rule (local store payload
    /// always, a remote reply only with `--peer-cache`), the guest block copy
    /// into `destination` when the read still wants it, then completion for
    /// every waiter. `remote` carries the transport milestones of a GET reply.
    pub fn publish(
        mut self,
        sources: &Sources,
        hash: Hash,
        remote: Option<crate::peer::metrics::Phases>,
        view: &View,
        block: u64,
        destination: Option<&mut [u8]>,
    ) -> io::Result<()> {
        self.cell
            .get_mut()
            .expect("unpublished fetch is unique")
            .remote = remote;
        sources.fill(hash, self.cell.bytes.as_slice(), remote.is_some())?;
        if let Some(destination) = destination {
            copy_chunk(view, block, self.cell.bytes.as_slice(), destination)?;
        }
        self.leader.complete(self.cell)
    }
}

impl Sources {
    /// Join the fetch registry for `hash`. The leader's cell (`chunk_bytes`,
    /// charged to `metadata`, pinning the read's credits) is allocated before
    /// the lookup, so no error path ever holds a leader; `wake` produces the
    /// descriptor a waiter is signalled through, and is called only then.
    #[expect(
        clippy::too_many_arguments,
        reason = "one registry join for both schedulers; every argument is the read's own state"
    )]
    pub fn join<F: FnOnce() -> io::Result<OwnedFd>>(
        &self,
        fetches: &Fetches,
        hash: Hash,
        chunk_bytes: usize,
        credits: &BudgetArc<Credits>,
        metadata: &Arc<Budget>,
        view: &View,
        block: u64,
        destination: &mut [u8],
        wake: Option<F>,
    ) -> io::Result<Joined> {
        let bytes = AlignedBuffer::try_new_in(chunk_bytes, allocator_api2::alloc::Global)?;
        let cell = BudgetArc::try_new(
            Fetched {
                remote: None,
                bytes,
                _credits: credits.clone(),
            },
            metadata,
        )?;
        let leader = match Registry::lookup_with(fetches, hash, wake)? {
            None => return Ok(Joined::Full),
            Some(Lookup::Waiter(waiter)) => return Ok(Joined::Wait(waiter)),
            Some(Lookup::Leader(leader)) => leader,
        };
        let mut fetch = Fetch { leader, cell };
        // A previous leader may have filled the cache between the miss and
        // this lookup; publish that value through the new cell as well.
        if let Some(bytes) = self.peek(&hash) {
            fetch.payload_mut().copy_from_slice(bytes.as_slice());
            copy_chunk(view, block, bytes.as_slice(), destination)?;
            fetch.leader.complete(fetch.cell)?;
            return Ok(Joined::Served);
        }
        Ok(Joined::Lead(fetch))
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
