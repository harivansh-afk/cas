//! Locked direct-IO manifests. The host governor reserves disk capacity before
//! invoking mutating operations and accounts physical output even on failure.
use super::{
    format::{Commit, Extent, FileHeader, Kind, Page, Root},
    tree::{Lookup, LookupState, Prepared, Tree},
};
use crate::{
    BLOCK_SIZE,
    aligned::AlignedBuffer,
    budget::{Budget, BudgetAllocator},
    chunk_index::Hash,
    direct,
    directory::Directory,
    encoding::require,
};
use std::{fs::File, io, path::Path, sync::Arc};

const NAME: &str = "manifest.v2";

mod snapshot;
pub use snapshot::{Snapshot, SnapshotInspection, SnapshotKey};

mod cache;
pub use cache::PageCache;

mod pins;
use pins::Pin;
pub use pins::Roots;

mod reclaim;
pub use reclaim::ReclaimedPages;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    pub store: [u8; 16],
    pub image: [u8; 16],
    pub image_bytes: u64,
}

impl Identity {
    fn initial(self) -> Commit {
        Commit {
            store: self.store,
            image: self.image,
            image_bytes: self.image_bytes,
            generation: 1,
            root: Root::default(),
            durable: 0,
        }
    }

    fn matches(self, commit: Commit) -> bool {
        self.store == commit.store
            && self.image == commit.image
            && self.image_bytes == commit.image_bytes
    }
}

pub struct Manifest {
    directory: Directory,
    file: Arc<File>,
    current: Commit,
    end: u64,
    metadata: Arc<Budget>,
    failed: bool,
    pin: Pin,
}

/// Immutable root/end with a pin on the actual locked IO file description.
/// Construction is restricted to a healthy manifest after sync.
#[derive(Clone)]
pub struct View {
    file: Arc<File>,
    commit: Commit,
    end: u64,
    metadata: Arc<Budget>,
    pin: Pin,
}

impl View {
    /// Equality includes the actual pinned file owner, exact COMMIT and end.
    pub fn same(&self, other: &Self) -> bool {
        self.commit == other.commit && self.end == other.end && self.owns(&other.file)
    }

    pub(crate) fn owns(&self, file: &Arc<File>) -> bool {
        Arc::ptr_eq(&self.file, file)
    }

    pub fn commit(&self) -> Commit {
        self.commit
    }
    pub fn end(&self) -> u64 {
        self.end
    }
    pub fn key(&self) -> SnapshotKey {
        SnapshotKey {
            commit: self.commit,
            end: self.end,
        }
    }
    pub fn file(&self) -> &File {
        &self.file
    }
    pub fn lookup(&self, block: u64) -> io::Result<Read> {
        Ok(Read {
            lookup: Lookup::new(self.commit, self.end, block)?,
            file: Arc::clone(&self.file),
            pin: self.pin.clone(),
        })
    }
    pub fn tree(&self) -> io::Result<Tree<'_, File>> {
        Tree::new(
            self.file.as_ref(),
            self.commit,
            self.end,
            Arc::clone(&self.metadata),
        )
    }
}

/// A page-driven lookup retaining its actual IO file through completion.
pub struct Read {
    lookup: Lookup,
    file: Arc<File>,
    pin: Pin,
}

impl Read {
    pub fn file(&self) -> &File {
        &self.file
    }
    pub fn state(&self) -> io::Result<LookupState> {
        self.lookup.state()
    }
    pub fn accept(&mut self, offset: u64, bytes: &[u8]) -> io::Result<LookupState> {
        self.lookup.accept(offset, bytes)
    }

    /// Drive cached pages through the same validation as actual page CQEs.
    pub fn cached(&mut self, cache: &PageCache) -> io::Result<LookupState> {
        let mut state = self.state()?;
        while let LookupState::Page { offset, .. } = state {
            let Some(page) = cache.get(&self.pin.page_key(offset)) else {
                break;
            };
            state = self.accept(offset, &page.bytes)?;
        }
        Ok(state)
    }

    pub fn accept_cached(
        &mut self,
        offset: u64,
        bytes: &[u8],
        cache: &PageCache,
    ) -> io::Result<LookupState> {
        self.accept(offset, bytes)?;
        match cache.fill(self.pin.page_key(offset), bytes) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::OutOfMemory => (),
            Err(error) => return Err(error),
        }
        self.cached(cache)
    }
}

impl Manifest {
    pub fn view(&self) -> io::Result<View> {
        self.healthy()?;
        Ok(View {
            file: Arc::clone(&self.file),
            commit: self.current,
            end: self.end,
            metadata: Arc::clone(&self.metadata),
            pin: self.pin.clone(),
        })
    }

    /// The directory already exists. Catalog publication follows this file and
    /// directory sync; an error leaves any created file for explicit recovery.
    pub fn create(path: &Path, identity: Identity, metadata: Arc<Budget>) -> io::Result<Self> {
        let current = identity.initial();
        current.validate(BLOCK_SIZE as u64)?;
        let end = (2 * BLOCK_SIZE) as u64;
        let pin = Pin::new(
            SnapshotKey {
                commit: current,
                end,
            },
            Arc::clone(&metadata),
        )?;
        let mut buffer =
            AlignedBuffer::try_new_in(2 * BLOCK_SIZE, BudgetAllocator::new(Arc::clone(&metadata)))?;
        FileHeader {
            store: identity.store,
            image_bytes: identity.image_bytes,
        }
        .encode_into(&mut buffer.as_mut_slice()[..BLOCK_SIZE])?;
        current.encode_into(&mut buffer.as_mut_slice()[BLOCK_SIZE..], BLOCK_SIZE as u64)?;
        let directory = Directory::open(path)?;
        let file = direct::open(&path.join(NAME), true)?;
        direct::Alignment::query(&file)?;
        direct::preallocate(&file, 0, (2 * BLOCK_SIZE) as u64)?;
        direct::write_bytes(&file, buffer.as_slice(), 0)?;
        file.sync_all()?;
        directory.sync()?;
        Ok(Self {
            directory,
            file: Arc::new(file),
            current,
            end,
            metadata,
            failed: false,
            pin,
        })
    }

    pub fn current(&self) -> Commit {
        self.current
    }

    pub fn end(&self) -> u64 {
        self.end
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    pub fn pinned_roots(&mut self, metadata: Arc<Budget>) -> io::Result<Roots<'_>> {
        self.healthy()?;
        self.pin.capture(&self.file, metadata)
    }

    /// The host establishes quiescence and reserves physical metadata output.
    /// Logical punch statistics do not establish filesystem free-space progress.
    pub fn reclaim_pages(&mut self, metadata: Arc<Budget>) -> io::Result<ReclaimedPages> {
        self.healthy()?;
        let prepared = reclaim::Prepared::new(self.pin.capture(&self.file, metadata)?)?;
        self.failed = true;
        let reclaimed = prepared.run()?;
        self.failed = false;
        Ok(reclaimed)
    }

    fn healthy(&self) -> io::Result<()> {
        crate::encoding::require_healthy(
            self.failed,
            "manifest owner failed; explicit recovery required",
        )
    }

    pub fn tree(&self) -> io::Result<Tree<'_, File>> {
        self.healthy()?;
        Tree::new(
            self.file.as_ref(),
            self.current,
            self.end,
            Arc::clone(&self.metadata),
        )
    }

    pub fn prepare(&self, changes: &[Extent], durable: u64) -> io::Result<Prepared> {
        self.prepare_with_metadata(changes, durable, Arc::clone(&self.metadata))
    }

    /// A host compactor uses its separate transaction metadata partition.
    pub fn prepare_with_metadata(
        &self,
        changes: &[Extent],
        durable: u64,
        metadata: Arc<Budget>,
    ) -> io::Result<Prepared> {
        self.healthy()?;
        Prepared::build(
            self.file.as_ref(),
            self.current,
            self.end,
            changes,
            durable,
            metadata,
        )
    }

    /// Chunks referenced by this transaction must already be verified and
    /// durable. The owner cannot publish another transaction through a failed IO.
    pub fn publish(&mut self, prepared: Prepared) -> io::Result<Commit> {
        self.healthy()?;
        require(
            prepared.previous() == self.current && prepared.offset() == self.end,
            "stale manifest transaction",
        )?;
        let end = self.end + prepared.bytes().len() as u64; // Prepared checked this range.
        let pin = self.pin.successor(SnapshotKey {
            commit: prepared.commit(),
            end,
        })?;
        self.failed = true;
        direct::preallocate(&self.file, self.end, prepared.bytes().len() as u64)?;
        direct::write_bytes(&self.file, prepared.bytes(), self.end)?;
        direct::sync_data(&self.file)?;
        self.end = end;
        self.current = prepared.commit();
        self.pin = pin;
        self.failed = false;
        Ok(self.current)
    }

    /// Read-only recovery inspection. The chunk visitor must reject missing or
    /// invalid durable data, including a hash hit whose payload is unavailable.
    pub fn inspect(
        path: &Path,
        identity: Identity,
        required_durable: u64,
        metadata: Arc<Budget>,
        mut verify_chunk: impl FnMut(Hash) -> io::Result<()>,
    ) -> io::Result<Inspection> {
        identity.initial().validate(BLOCK_SIZE as u64)?;
        let directory = Directory::open(path)?;
        let file = direct::open(&path.join(NAME), false)?;
        direct::Alignment::query(&file)?;
        let selected = select(&file, identity, required_durable, Arc::clone(&metadata))?;
        Tree::new(&file, selected.commit, selected.end, Arc::clone(&metadata))?.walk(|extent| {
            if let Some(hash) = extent.hash {
                verify_chunk(hash)?;
            }
            Ok(())
        })?;
        let pin = Pin::new(
            SnapshotKey {
                commit: selected.commit,
                end: selected.end,
            },
            Arc::clone(&metadata),
        )?;
        Ok(Inspection {
            manifest: Self {
                directory,
                file: Arc::new(file),
                current: selected.commit,
                end: selected.end,
                metadata,
                failed: true, // Recovery sync precedes use or publication of D.
                pin,
            },
            selected,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Selected {
    pub commit: Commit,
    pub end: u64,
    pub file_bytes: u64,
    pub scanned_pages: u64,
    pub incomplete_commits: u64,
}

/// Owns the actual file lock. No repair is exposed until metadata and every
/// required chunk have passed inspection.
pub struct Inspection {
    manifest: Manifest,
    selected: Selected,
}

impl Inspection {
    pub(crate) fn file(&self) -> Arc<File> {
        Arc::clone(&self.manifest.file)
    }

    pub fn selected(&self) -> Selected {
        self.selected
    }

    pub fn validate_recovery(&self, repair: crate::space::Recovery<'_>) -> io::Result<()> {
        repair.validate_file(&self.manifest.file)?;
        repair.validate_output(Directory::archive_bytes(
            &self.manifest.file,
            self.manifest.end,
        )?)
    }

    pub fn recover(self) -> io::Result<Manifest> {
        self.recover_with(crate::space::Recovery::default())
    }

    pub fn recover_with(mut self, repair: crate::space::Recovery<'_>) -> io::Result<Manifest> {
        self.validate_recovery(repair)?;
        let manifest = &mut self.manifest;
        if self.selected.file_bytes > manifest.end {
            repair.archive(&manifest.directory, NAME, &manifest.file, manifest.end)?;
        }
        // A complete unsynced transaction may have survived. Stabilize it even
        // when inspection found no rejected suffix to truncate.
        repair.output(0, || {
            if self.selected.file_bytes > manifest.end {
                manifest.file.set_len(manifest.end)?;
            }
            direct::sync_data(&manifest.file)
        })?;
        manifest.failed = false;
        Ok(self.manifest)
    }
}

fn select(
    file: &File,
    identity: Identity,
    required_durable: u64,
    metadata: Arc<Budget>,
) -> io::Result<Selected> {
    let file_bytes = file.metadata()?.len();
    require(
        ((2 * BLOCK_SIZE) as u64..=i64::MAX as u64).contains(&file_bytes),
        "manifest file length",
    )?;
    let mut scratch =
        AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(Arc::clone(&metadata)))?;
    direct::read_bytes(file, scratch.as_mut_slice(), 0)?;
    let header = FileHeader::decode(scratch.as_slice())?;
    require(
        header.store == identity.store && header.image_bytes == identity.image_bytes,
        "manifest FILE does not match catalog",
    )?;
    let mut incomplete_commits = 0;
    let mut newer = None::<Commit>;
    for (scanned, block) in (1..file_bytes / BLOCK_SIZE as u64).rev().enumerate() {
        let offset = block * BLOCK_SIZE as u64;
        direct::read_bytes(file, scratch.as_mut_slice(), offset)?;
        let Ok(page) = Page::decode(scratch.as_slice(), offset, identity.image_bytes) else {
            continue; // A torn page or a hole has no usable COMMIT.
        };
        if page.kind() != Kind::Commit {
            continue;
        }
        let commit = page.commit()?;
        require(
            identity.matches(commit),
            "manifest COMMIT does not match catalog",
        )?;
        if let Some(newer) = newer {
            require(
                commit.generation < newer.generation && commit.durable <= newer.durable,
                "manifest COMMIT order regressed",
            )?;
        }
        newer = Some(commit);
        let end = offset + BLOCK_SIZE as u64;
        let structural = Tree::new(file, commit, end, Arc::clone(&metadata))?.walk(|_| Ok(()));
        match structural {
            Ok(()) => {
                require(
                    commit.durable >= required_durable,
                    "manifest below required D",
                )?;
                return Ok(Selected {
                    commit,
                    end,
                    file_bytes,
                    scanned_pages: scanned as u64 + 1,
                    incomplete_commits,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                incomplete_commits += 1;
            }
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "manifest has no complete COMMIT",
    ))
}

#[cfg(test)]
mod tests;
