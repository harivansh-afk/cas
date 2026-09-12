mod pins;
pub(super) use pins::Pins;

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::sync::{Arc, atomic::AtomicU64};

use super::format::SegmentHeader;
use crate::budget::{Budget, BudgetAllocator};
pub(super) use crate::directory::Directory;
use crate::{BLOCK_SIZE, aligned::AlignedBuffer, direct};
use allocator_api2::vec::Vec;

pub(super) use crate::segments::{name, number};

pub(super) struct Candidates {
    pub highest: u64,
    pub files: Vec<(u64, Arc<File>), BudgetAllocator>,
}

pub(super) fn candidates(directory: &Directory, metadata: Arc<Budget>) -> io::Result<Candidates> {
    let mut highest = 0;
    let mut files = Vec::new_in(BudgetAllocator::new(metadata));
    for entry in fs::read_dir(&directory.path)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(value) = number(&name) {
            highest = highest.max(value);
            if name != super::segment::name(value) {
                return Err(io::Error::other("invalid segment filename"));
            }
            files.try_reserve(1).map_err(|_| {
                io::Error::new(io::ErrorKind::OutOfMemory, "WAL candidate table exhausted")
            })?;
            // Retain the actual locked file, including rejected suffixes.
            files.push((value, Arc::new(direct::open(&entry.path(), false)?)));
        }
    }
    let archive = directory.path.join("rejected");
    if archive.exists() {
        for entry in fs::read_dir(archive)? {
            if let Some(value) = number(&entry?.file_name().to_string_lossy()) {
                highest = highest.max(value);
            }
        }
    }
    files.sort_unstable_by_key(|(number, _)| *number);
    Ok(Candidates { highest, files })
}

/// None selects a fresh epoch from the number actually assigned under the
/// shared allocator lock. Existing epochs survive rollover/live recovery.
pub(super) fn create(
    directory: &Directory,
    config: super::Config,
    tickets: Option<&crate::segments::Tickets>,
    highest: u64,
    epoch: Option<u64>,
    preceding: u64,
    pins: Pins,
) -> io::Result<Arc<Segment>> {
    let create = |number| {
        Segment::create(
            directory,
            config.header(epoch.unwrap_or(number), number, preceding),
            pins,
        )
    };
    match tickets {
        Some(tickets) => tickets.allocate(create),
        None => create(
            highest
                .checked_add(1)
                .ok_or_else(|| io::Error::other("segment tickets exhausted"))?,
        ),
    }
}

#[derive(Debug)]
pub(super) struct Segment {
    pub file: Arc<File>,
    pub header: SegmentHeader,
    pub alignment: direct::Alignment,
    pub pins: Pins,
    pub end: AtomicU64,
}

impl Segment {
    pub fn create(
        directory: &Directory,
        header: SegmentHeader,
        pins: Pins,
    ) -> io::Result<Arc<Self>> {
        let buffer = header.encode().map_err(io::Error::other)?;
        let file = direct::open(&directory.path.join(name(header.number)), true)?;
        let alignment = direct::Alignment::query(&file)?;
        direct::preallocate(&file, 0, header.capacity)?;
        direct::write(&file, &buffer, 0)?;
        direct::sync_all(&file)?;
        directory.sync()?;
        Ok(Arc::new(Self {
            file: Arc::new(file),
            header,
            alignment,
            pins,
            end: AtomicU64::new(BLOCK_SIZE as u64),
        }))
    }

    pub fn open(
        number: u64,
        file: Arc<File>,
        metadata: Arc<crate::budget::Budget>,
    ) -> io::Result<Arc<Self>> {
        let alignment = direct::Alignment::query(&file)?;
        let mut buffer =
            AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(Arc::clone(&metadata)))?;
        direct::read_bytes(&file, buffer.as_mut_slice(), 0)?;
        let header = SegmentHeader::decode(buffer.as_slice()).map_err(io::Error::other)?;
        if header.number != number {
            return Err(io::Error::other("segment filename/header mismatch"));
        }
        let pins = Pins::new(header.capacity, metadata)?;
        let end = AtomicU64::new(file.metadata()?.len());
        Ok(Arc::new(Self {
            file,
            header,
            alignment,
            pins,
            end,
        }))
    }

    pub fn allocated_bytes(&self) -> io::Result<u64> {
        Ok(self.file.metadata()?.blocks() * 512)
    }
}
