use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::format::SegmentHeader;
use crate::{BLOCK_SIZE, aligned::AlignedBuffer, direct};

pub(super) fn name(number: u64) -> String {
    format!("segment-{number:020}.v2")
}

pub(super) fn number(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("segment-")?.get(..20)?;
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|value| *value != 0)
}

pub(super) struct Directory {
    pub path: PathBuf,
    file: File,
}

pub(super) struct Candidates {
    pub highest: u64,
    pub files: Vec<(u64, Arc<File>)>,
}

impl Directory {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)?;
        file.try_lock().map_err(io::Error::from)?;
        Ok(Self {
            path: path.to_owned(),
            file,
        })
    }

    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }

    pub fn candidates(&self) -> io::Result<Candidates> {
        let mut highest = 0;
        let mut paths = Vec::new();
        for entry in fs::read_dir(&self.path)? {
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
        let archive = self.path.join("rejected");
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

    pub fn archive(&self, number: u64, file: &File, offset: u64) -> io::Result<PathBuf> {
        use std::io::Write;
        use std::os::unix::fs::FileExt;
        let archive = self.path.join("rejected");
        if !archive.exists() {
            fs::create_dir(&archive)?;
            self.sync()?;
        }
        let mut attempt = 0u64;
        let (path, mut output) = loop {
            let path = archive.join(format!("{}-from-{offset:020}-{attempt}", name(number)));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => break (path, file),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    attempt = attempt
                        .checked_add(1)
                        .ok_or_else(|| io::Error::other("archive IDs exhausted"))?;
                }
                Err(error) => return Err(error),
            }
        };
        // Archive through a separate buffered read descriptor. The locked direct
        // IO description remains alive; only archival evidence uses buffered IO.
        let input = File::open(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
        let mut position = offset;
        let mut buffer = [0; 64 * 1024];
        let length = input.metadata()?.len();
        while position < length {
            let wanted = buffer.len().min((length - position) as usize);
            let read = input.read_at(&mut buffer[..wanted], position)?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "short archive read",
                ));
            }
            output.write_all(&buffer[..read])?;
            position += read as u64;
        }
        output.sync_all()?;
        File::open(&archive)?.sync_all()?;
        Ok(path)
    }
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
        let capacity = i64::try_from(header.capacity).map_err(io::Error::other)?;
        // SAFETY: fallocate takes a live descriptor and validated positive range;
        // KEEP_SIZE reserves space without publishing unwritten records as EOF.
        if unsafe { libc::fallocate(file.as_raw_fd(), libc::FALLOC_FL_KEEP_SIZE, 0, capacity) } != 0
        {
            return Err(io::Error::last_os_error());
        }
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
