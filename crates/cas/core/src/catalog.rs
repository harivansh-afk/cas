//! Atomic catalog membership; dependency inspection and physical permits belong
//! to the host coordinator. See docs/catalog.md for the publication contract.
use crate::{
    budget::Budget, direct, directory::Directory, encoding::require, manifest::file::SnapshotKey,
    segments::Tickets,
};
use std::{fs, fs::File, io, sync::Arc};

mod format;
pub use format::Contents;

const DIRECTORY: &str = "catalog";
const NAME: &str = "catalog.v2";

pub type Id = [u8; 16];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image { image_bytes: u64 },
    Snapshot(SnapshotKey),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: Id,
    pub kind: Kind,
}

#[derive(Clone, Copy, Debug)]
pub enum Change {
    Insert(Entry),
    Remove(Id),
}

struct Owner {
    _tickets: Arc<Tickets>,
    directory: Directory,
}

pub struct Catalog {
    owner: Arc<Owner>,
    file: File,
    contents: Contents,
    failed: bool,
}

impl Catalog {
    /// Create the initial empty catalog. Failed output remains for inspection;
    /// the caller reserves and accounts all allocation, including this directory.
    pub fn create(tickets: Arc<Tickets>, store: Id, metadata: Arc<Budget>) -> io::Result<Self> {
        let contents = Contents::empty(store, metadata)?;
        Self::create_contents(tickets, contents)
    }

    fn create_contents(tickets: Arc<Tickets>, contents: Contents) -> io::Result<Self> {
        let path = tickets.root().join(DIRECTORY);
        fs::create_dir(&path)?;
        let directory = Directory::open(&path)?;
        direct::sync_all(tickets.root_file())?;
        let owner = Arc::new(Owner {
            _tickets: tickets,
            directory,
        });
        let file = write(&owner, &contents)?;
        Ok(Self {
            owner,
            file,
            contents,
            failed: false,
        })
    }

    /// Reads only the canonical file. Pending publications are retained, never
    /// selected or removed. All shared dependencies must pass before recover().
    pub fn inspect(
        tickets: Arc<Tickets>,
        store: Id,
        metadata: Arc<Budget>,
    ) -> io::Result<Inspection> {
        let directory = Directory::open(&tickets.root().join(DIRECTORY))?;
        let file = direct::open(&directory.path().join(NAME), false)?;
        direct::Alignment::query(&file)?;
        let contents = Contents::read(&file, store, metadata)?;
        Ok(Inspection {
            owner: Arc::new(Owner {
                _tickets: tickets,
                directory,
            }),
            file,
            contents,
        })
    }

    pub fn contents(&self) -> &Contents {
        &self.contents
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    fn healthy(&self) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::other(
                "catalog failed; explicit recovery required",
            ));
        }
        Ok(())
    }

    pub fn prepare(&self, change: Change) -> io::Result<Prepared> {
        self.healthy()?;
        Ok(Prepared {
            owner: Arc::clone(&self.owner),
            generation: self.contents.generation(),
            contents: self.contents.changed(change)?,
        })
    }

    /// Newly referenced files must already be durable. Removal must complete
    /// this publication before lifecycle unlinks the removed dependency files.
    pub fn publish(&mut self, prepared: Prepared) -> io::Result<()> {
        self.healthy()?;
        require(
            Arc::ptr_eq(&self.owner, &prepared.owner)
                && self.contents.generation() == prepared.generation,
            "stale or foreign catalog preparation",
        )?;
        self.failed = true;
        let file = write(&self.owner, &prepared.contents)?;
        self.file = file;
        self.contents = prepared.contents;
        self.failed = false;
        Ok(())
    }
}

/// Fully encoded initial membership, prepared before dependency creation.
pub struct Initial {
    contents: Contents,
}

impl Initial {
    pub fn prepare(
        store: Id,
        entries: impl ExactSizeIterator<Item = Entry>,
        metadata: Arc<Budget>,
    ) -> io::Result<Self> {
        Ok(Self {
            contents: Contents::initial(store, entries, metadata)?,
        })
    }

    pub fn contents(&self) -> &Contents {
        &self.contents
    }

    pub fn output_bytes(&self) -> usize {
        self.contents.bytes().len()
    }

    /// The coordinator has synced every dependency and reserved physical output.
    pub fn publish(self, tickets: Arc<Tickets>) -> io::Result<Catalog> {
        Catalog::create_contents(tickets, self.contents)
    }
}

pub struct Prepared {
    owner: Arc<Owner>,
    generation: u64,
    contents: Contents,
}

impl Prepared {
    /// Encoded file output only; the physical governor must also reserve the
    /// dedicated filesystem's metadata overhead and retain any failed output.
    pub fn output_bytes(&self) -> usize {
        self.contents.bytes().len()
    }
}

pub struct Inspection {
    owner: Arc<Owner>,
    file: File,
    contents: Contents,
}

impl Inspection {
    pub fn validate_recovery(&self, repair: crate::space::Recovery<'_>) -> io::Result<()> {
        repair.validate_tickets(Some(&self.owner._tickets))?;
        repair.validate_file(&self.file)?;
        repair.validate_output(0)
    }

    pub fn contents(&self) -> &Contents {
        &self.contents
    }

    /// The host has inspected every catalog dependency and saved image P.
    pub fn recover(self) -> io::Result<Catalog> {
        direct::sync_all(&self.file)?;
        self.owner.directory.sync()?;
        Ok(Catalog {
            owner: self.owner,
            file: self.file,
            contents: self.contents,
            failed: false,
        })
    }
}

fn write(owner: &Owner, contents: &Contents) -> io::Result<File> {
    let directory = &owner.directory;
    let mut attempt = 0u64;
    let (name, file) = loop {
        let name = format!("pending-{:020}-{attempt:020}.v2", contents.generation());
        match direct::open(&directory.path().join(&name), true) {
            Ok(file) => break (name, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                attempt = attempt
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("catalog attempt IDs exhausted"))?;
            }
            Err(error) => return Err(error),
        }
    };
    direct::Alignment::query(&file)?;
    direct::preallocate(&file, 0, contents.bytes().len() as u64)?;
    direct::write_bytes(&file, contents.bytes(), 0)?;
    direct::sync_all(&file)?;
    directory.replace(&name, NAME)?;
    directory.sync()?;
    Ok(file)
}

#[cfg(test)]
mod tests;
