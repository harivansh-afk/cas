use std::fs::{self, File};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

use super::format::SegmentHeader;
pub(super) use crate::directory::Directory;
use crate::{BLOCK_SIZE, aligned::AlignedBuffer, direct};

pub(super) use crate::segments::{name, number};

pub(super) struct Candidates {
    pub highest: u64,
    pub files: Vec<(u64, Arc<File>)>,
}

pub(super) fn candidates(directory: &Directory) -> io::Result<Candidates> {
    let mut highest = 0;
    let mut paths = Vec::new();
    for entry in fs::read_dir(&directory.path)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(value) = number(&name) {
            highest = highest.max(value);
            if name != super::segment::name(value) {
                return Err(io::Error::other("invalid segment filename"));
            }
            paths.push((value, entry.path()));
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
    paths.sort_by_key(|(number, _)| *number);
    // Lock the actual file descriptions before recovery can mutate anything.
    let files = paths
        .into_iter()
        .map(|(number, path)| Ok((number, Arc::new(direct::open(&path, false)?))))
        .collect::<io::Result<Vec<_>>>()?;
    Ok(Candidates { highest, files })
}

#[derive(Debug)]
pub(super) struct Segment {
    pub file: Arc<File>,
    pub header: SegmentHeader,
    pub alignment: direct::Alignment,
}

impl Segment {
    pub fn create(directory: &Directory, header: SegmentHeader) -> io::Result<Arc<Self>> {
        let buffer = header.encode().map_err(io::Error::other)?;
        let file = direct::open(&directory.path.join(name(header.number)), true)?;
        let alignment = direct::Alignment::query(&file)?;
        direct::preallocate(&file, 0, header.capacity)?;
        direct::write(&file, &buffer, 0)?;
        file.sync_all()?;
        directory.sync()?;
        Ok(Arc::new(Self {
            file: Arc::new(file),
            header,
            alignment,
        }))
    }

    pub fn open(number: u64, file: Arc<File>) -> io::Result<Arc<Self>> {
        let alignment = direct::Alignment::query(&file)?;
        let mut buffer = AlignedBuffer::new(BLOCK_SIZE);
        direct::read(&file, &mut buffer, 0)?;
        let header = SegmentHeader::decode(buffer.as_slice()).map_err(io::Error::other)?;
        if header.number != number {
            return Err(io::Error::other("segment filename/header mismatch"));
        }
        Ok(Arc::new(Self {
            file,
            header,
            alignment,
        }))
    }

    pub fn allocated_bytes(&self) -> io::Result<u64> {
        Ok(self.file.metadata()?.blocks() * 512)
    }
}
