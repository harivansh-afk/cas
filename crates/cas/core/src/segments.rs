//! Store-wide tickets backed by retained segment names, never a counter ledger.
use crate::{
    budget::{Budget, BudgetAllocator},
    chunk_index::MAX_SEGMENT,
    directory::Directory,
    encoding::require,
};
use hashbrown::HashTable;
use std::{
    fs, io,
    path::Path,
    sync::{Arc, Mutex},
};

pub(crate) fn name(number: u64) -> String {
    format!("segment-{number:020}.v2")
}

// Archives retain the original name as a prefix. Callers distinguish a live
// canonical filename from a retained archive before accepting it as live data.
pub(crate) fn number(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("segment-")?.get(..20)?;
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|value| *value != 0)
}

#[derive(Debug, Clone, Copy)]
pub struct Status {
    pub highest: u64,
    pub failed: bool,
}

/// Retains the root lock for the host owner. Actual IO files have their own
/// locks, which continue excluding recovery while kernel IO references survive.
pub struct Tickets {
    directory: Directory,
    state: Mutex<Status>,
}

impl Tickets {
    pub(crate) fn root_file(&self) -> &fs::File {
        self.directory.file()
    }

    /// Inspect existing names without creating directories or repairing files.
    /// Individual stores/logs still validate every header and required prefix.
    pub fn open(root: &Path, metadata: Arc<Budget>) -> io::Result<Arc<Self>> {
        let directory = Directory::open(root)?;
        let mut scan = Scan {
            highest: 0,
            live: HashTable::new_in(BudgetAllocator::new(metadata)),
        };
        scan.directory(&root.join("chunks"))?;
        if let Some(images) = entries(&root.join("images"))? {
            for entry in images {
                let entry = entry?;
                let image = entry.file_name();
                let image = image
                    .to_str()
                    .ok_or_else(|| io::Error::other("image directory encoding"))?;
                require(
                    image.len() == 32
                        && image
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                        && entry.file_type()?.is_dir(),
                    "invalid image directory",
                )?;
                scan.directory(&entry.path().join("staging"))?;
            }
        }
        Ok(Arc::new(Self {
            directory,
            state: Mutex::new(Status {
                highest: scan.highest,
                failed: false,
            }),
        }))
    }

    pub fn status(&self) -> Status {
        *self.state.lock().expect("segment allocator mutex poisoned")
    }

    pub fn device(&self) -> io::Result<u64> {
        use std::os::unix::fs::MetadataExt;
        Ok(self.directory.file().metadata()?.dev())
    }

    pub fn root(&self) -> &Path {
        self.directory.path()
    }

    /// `create` must retain the assigned filename and sync the header, file and
    /// containing directory before returning success. It may not call Tickets
    /// recursively. Error poisons this owner; restart must inspect retained names.
    pub fn allocate<T>(&self, create: impl FnOnce(u64) -> io::Result<T>) -> io::Result<T> {
        let mut state = self.state.lock().expect("segment allocator mutex poisoned");
        if state.failed {
            return Err(io::Error::other(
                "segment allocator failed; explicit recovery required",
            ));
        }
        let next = state
            .highest
            .checked_add(1)
            .filter(|number| *number <= MAX_SEGMENT)
            .ok_or_else(|| io::Error::other("segment tickets exhausted"))?;
        state.failed = true;
        let owner = create(next)?;
        state.highest = next;
        state.failed = false;
        Ok(owner)
    }
}

struct Scan {
    highest: u64,
    live: HashTable<u64, BudgetAllocator>,
}

impl Scan {
    fn directory(&mut self, path: &Path) -> io::Result<()> {
        let Some(files) = entries(path)? else {
            return Ok(());
        };
        for entry in files {
            let entry = entry?;
            let filename = entry.file_name();
            if filename == "rejected" {
                self.archives(&entry.path())?;
                continue;
            }
            let filename = filename
                .to_str()
                .ok_or_else(|| io::Error::other("segment filename encoding"))?;
            let number = self.observe(filename)?;
            require(
                filename == name(number) && entry.file_type()?.is_file(),
                "invalid live segment filename/type",
            )?;
            require(
                self.live.find(number, |old| *old == number).is_none(),
                "duplicate live segment ticket",
            )?;
            self.live.try_reserve(1, |number| *number).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "segment scan metadata budget exhausted",
                )
            })?;
            self.live.insert_unique(number, number, |number| *number);
        }
        Ok(())
    }

    fn archives(&mut self, path: &Path) -> io::Result<()> {
        if let Some(files) = entries(path)? {
            for entry in files {
                let entry = entry?;
                let filename = entry.file_name();
                let filename = filename
                    .to_str()
                    .ok_or_else(|| io::Error::other("archive filename encoding"))?;
                let number = self.observe(filename)?;
                require(
                    filename.starts_with(&format!("{}-from-", name(number)))
                        && entry.file_type()?.is_file(),
                    "invalid segment archive filename/type",
                )?;
            }
        }
        Ok(())
    }

    fn observe(&mut self, filename: &str) -> io::Result<u64> {
        let number = number(filename)
            .filter(|number| *number <= MAX_SEGMENT)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid segment ticket"))?;
        self.highest = self.highest.max(number);
        Ok(number)
    }
}

fn entries(path: &Path) -> io::Result<Option<fs::ReadDir>> {
    match path.symlink_metadata() {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
        Ok(metadata) => {
            require(
                metadata.is_dir(),
                "segment namespace requires a real directory",
            )?;
            Ok(Some(fs::read_dir(path)?))
        }
    }
}

#[cfg(test)]
mod tests;
