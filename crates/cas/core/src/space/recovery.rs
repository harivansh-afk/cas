//! Recovery uses the same exclusive physical borrower as runtime background IO.
use super::Governor;
use crate::{directory::Directory, segments::Tickets};
use std::{fmt, fs::File, io, path::PathBuf, sync::Arc};

pub const METADATA_MARGIN: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Default)]
pub struct Recovery<'a> {
    physical: Option<&'a Arc<Governor>>,
}

impl<'a> Recovery<'a> {
    pub(crate) fn physical(&self) -> Option<Arc<Governor>> {
        self.physical.cloned()
    }

    pub fn governed(physical: &'a Arc<Governor>) -> Self {
        Self {
            physical: Some(physical),
        }
    }

    pub fn validate_output(&self, bytes: u64) -> io::Result<()> {
        self.output_bytes(bytes).map(|_| ())
    }

    fn output_bytes(&self, bytes: u64) -> io::Result<u64> {
        let total = bytes
            .checked_add(METADATA_MARGIN)
            .ok_or_else(|| io::Error::other("recovery output bound overflow"))?;
        if self
            .physical
            .is_some_and(|physical| total > physical.limits().reserve)
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "recovery output exceeds progress reserve",
            ));
        }
        Ok(total)
    }

    pub fn output<T, E: From<io::Error> + fmt::Display>(
        &self,
        bytes: u64,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let bytes = self.output_bytes(bytes)?;
        match self.physical {
            None => operation(),
            Some(physical) => physical.background(bytes)?.run_with(operation),
        }
    }

    pub(crate) fn validate_file(&self, file: &File) -> io::Result<()> {
        self.physical
            .map_or(Ok(()), |physical| physical.validate_file(file))
    }

    pub(crate) fn validate_tickets(&self, tickets: Option<&Arc<Tickets>>) -> io::Result<()> {
        if self
            .physical
            .is_some_and(|physical| tickets.is_none_or(|tickets| !physical.uses_tickets(tickets)))
        {
            return Err(io::Error::other("recovery physical owner differs"));
        }
        Ok(())
    }

    pub(crate) fn archive(
        &self,
        directory: &Directory,
        name: &str,
        file: &File,
        offset: u64,
    ) -> io::Result<PathBuf> {
        self.validate_file(file)?;
        self.output(Directory::archive_bytes(file, offset)?, || {
            directory.archive(name, file, offset)
        })
    }
}
