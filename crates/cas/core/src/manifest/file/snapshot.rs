//! Immutable exact-root reflinks, inspected before shared dependency repair.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotKey {
    pub commit: Commit,
    pub end: u64,
}

impl SnapshotKey {
    fn validate(self) -> io::Result<()> {
        require(
            self.end >= (2 * BLOCK_SIZE) as u64
                && self.end <= i64::MAX as u64
                && self.end.is_multiple_of(BLOCK_SIZE as u64),
            "snapshot end is invalid",
        )?;
        self.commit.validate(self.end - BLOCK_SIZE as u64)
    }

    fn verify(self, file: &File, scratch: &mut [u8]) -> io::Result<()> {
        self.validate()?;
        require(
            file.metadata()?.len() == self.end,
            "snapshot EOF differs from its catalog key",
        )?;
        direct::read_bytes(file, scratch, 0)?;
        let header = FileHeader::decode(scratch)?;
        require(
            header.store == self.commit.store && header.image_bytes == self.commit.image_bytes,
            "snapshot FILE differs from its catalog key",
        )?;
        let offset = self.end - BLOCK_SIZE as u64;
        direct::read_bytes(file, scratch, offset)?;
        let commit = Page::decode(scratch, offset, self.commit.image_bytes)?.commit()?;
        require(
            commit == self.commit,
            "snapshot COMMIT differs from its catalog key",
        )
    }
}

pub struct Snapshot {
    _directory: Directory,
    view: View,
}

impl Snapshot {
    pub fn key(&self) -> SnapshotKey {
        SnapshotKey {
            commit: self.view.commit,
            end: self.view.end,
        }
    }

    pub fn view(&self) -> View {
        self.view.clone()
    }

    /// The destination directory already exists. The host has established its
    /// snapshot cut and reserved disk output; catalog publication follows sync.
    pub fn create(source: &View, path: &Path, metadata: Arc<Budget>) -> io::Result<Self> {
        let mut scratch =
            AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(Arc::clone(&metadata)))?;
        let copy = Reflink::create(source, path, scratch.as_mut_slice())?;
        let (directory, file) = copy.sync()?;
        Ok(Self {
            _directory: directory,
            view: View {
                file,
                commit: source.commit,
                end: source.end,
                metadata,
            },
        })
    }

    /// Immutable catalog entries never select an alternative root or repair a
    /// suffix. Every dependency is checked before exposing a recoverable owner.
    pub fn inspect(
        path: &Path,
        key: SnapshotKey,
        metadata: Arc<Budget>,
        mut verify_chunk: impl FnMut(Hash) -> io::Result<()>,
    ) -> io::Result<SnapshotInspection> {
        key.validate()?;
        let mut scratch =
            AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(Arc::clone(&metadata)))?;
        let directory = Directory::open(path)?;
        let file = direct::open(&path.join(NAME), false)?;
        direct::Alignment::query(&file)?;
        key.verify(&file, scratch.as_mut_slice())?;
        drop(scratch);
        Tree::new(&file, key.commit, key.end, Arc::clone(&metadata))?.walk(|extent| {
            if let Some(hash) = extent.hash {
                verify_chunk(hash)?;
            }
            Ok(())
        })?;
        Ok(SnapshotInspection {
            directory,
            file: Arc::new(file),
            key,
            metadata,
        })
    }
}

pub struct SnapshotInspection {
    directory: Directory,
    file: Arc<File>,
    key: SnapshotKey,
    metadata: Arc<Budget>,
}

impl SnapshotInspection {
    pub fn selected(&self) -> SnapshotKey {
        self.key
    }

    /// The coordinator first validates every shared dependency and saved P.
    pub fn recover(self) -> io::Result<Snapshot> {
        direct::sync_data(&self.file)?;
        Ok(Snapshot {
            _directory: self.directory,
            view: View {
                file: self.file,
                commit: self.key.commit,
                end: self.key.end,
                metadata: self.metadata,
            },
        })
    }
}

impl Manifest {
    pub fn clone_snapshot(
        source: &Snapshot,
        path: &Path,
        identity: Identity,
        metadata: Arc<Budget>,
    ) -> io::Result<Self> {
        let key = source.key();
        require(
            identity.store == key.commit.store
                && identity.image_bytes == key.commit.image_bytes
                && identity.image != key.commit.image,
            "clone requires a new image in the same store and geometry",
        )?;
        let current = Commit {
            root: key.commit.root,
            ..identity.initial()
        };
        current.validate(key.end)?;
        let end = key
            .end
            .checked_add(BLOCK_SIZE as u64)
            .filter(|end| *end <= i64::MAX as u64)
            .ok_or_else(|| io::Error::other("clone manifest end exhausted"))?;
        let mut scratch =
            AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(Arc::clone(&metadata)))?;
        let copy = Reflink::create(&source.view, path, scratch.as_mut_slice())?;
        current.encode_into(scratch.as_mut_slice(), key.end)?;
        direct::preallocate(&copy.file, key.end, BLOCK_SIZE as u64)?;
        direct::write_bytes(&copy.file, scratch.as_slice(), key.end)?;
        let (directory, file) = copy.sync()?;
        Ok(Self {
            directory,
            file,
            current,
            end,
            metadata,
            failed: false,
        })
    }
}

struct Reflink {
    directory: Directory,
    file: File,
}

impl Reflink {
    fn create(source: &View, path: &Path, scratch: &mut [u8]) -> io::Result<Self> {
        let key = SnapshotKey {
            commit: source.commit,
            end: source.end,
        };
        key.validate()?;
        let directory = Directory::open(path)?;
        let file = direct::open(&path.join(NAME), true)?;
        direct::Alignment::query(&file)?;
        direct::reflink(source.file(), &file)?;
        file.set_len(key.end)?;
        key.verify(&file, scratch)?;
        Ok(Self { directory, file })
    }

    fn sync(self) -> io::Result<(Directory, Arc<File>)> {
        direct::sync_all(&self.file)?;
        self.directory.sync()?;
        Ok((self.directory, Arc::new(self.file)))
    }
}
