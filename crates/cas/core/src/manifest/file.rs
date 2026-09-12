//! Locked direct-IO manifests. The host governor reserves disk capacity before
//! invoking mutating operations and accounts physical output even on failure.
use super::{
    format::{Commit, Extent, FileHeader, Kind, Page, Root},
    tree::{Prepared, Tree},
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
    file: File,
    current: Commit,
    end: u64,
    metadata: Arc<Budget>,
    failed: bool,
}

impl Manifest {
    /// The directory already exists. Catalog publication follows this file and
    /// directory sync; an error leaves any created file for explicit recovery.
    pub fn create(path: &Path, identity: Identity, metadata: Arc<Budget>) -> io::Result<Self> {
        let current = identity.initial();
        current.validate(BLOCK_SIZE as u64)?;
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
            file,
            current,
            end: (2 * BLOCK_SIZE) as u64,
            metadata,
            failed: false,
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

    fn healthy(&self) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::other(
                "manifest owner failed; explicit recovery required",
            ));
        }
        Ok(())
    }

    pub fn tree(&self) -> io::Result<Tree<'_, File>> {
        self.healthy()?;
        Tree::new(
            &self.file,
            self.current,
            self.end,
            Arc::clone(&self.metadata),
        )
    }

    pub fn prepare(&self, changes: &[Extent], durable: u64) -> io::Result<Prepared> {
        self.healthy()?;
        Prepared::build(
            &self.file,
            self.current,
            self.end,
            changes,
            durable,
            Arc::clone(&self.metadata),
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
        self.failed = true;
        direct::preallocate(&self.file, self.end, prepared.bytes().len() as u64)?;
        direct::write_bytes(&self.file, prepared.bytes(), self.end)?;
        direct::sync_data(&self.file)?;
        self.end += prepared.bytes().len() as u64; // Prepared checked the full range.
        self.current = prepared.commit();
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
        Ok(Inspection {
            manifest: Self {
                directory,
                file,
                current: selected.commit,
                end: selected.end,
                metadata,
                failed: true, // Recovery sync precedes use or publication of D.
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
    pub fn selected(&self) -> Selected {
        self.selected
    }

    pub fn recover(mut self) -> io::Result<Manifest> {
        let manifest = &mut self.manifest;
        if self.selected.file_bytes > manifest.end {
            manifest
                .directory
                .archive(NAME, &manifest.file, manifest.end)?;
            manifest.file.set_len(manifest.end)?;
        }
        // A complete unsynced transaction may have survived. Stabilize it even
        // when inspection found no rejected suffix to truncate.
        direct::sync_data(&manifest.file)?;
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
