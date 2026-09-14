//! Locked storage directories and durable preservation of rejected suffixes.
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub(crate) struct Directory {
    path: PathBuf,
    file: File,
}

impl Directory {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file(&self) -> &File {
        &self.file
    }

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

    pub fn duplicate(&self) -> io::Result<Self> {
        Ok(Self {
            path: self.path.clone(),
            file: self.file.try_clone()?,
        })
    }

    pub fn sync(&self) -> io::Result<()> {
        let _measurement = crate::io_metrics::measure(0, |c| &mut c.sync);
        #[cfg(test)]
        if crate::direct::faults::take(crate::direct::faults::Fault::DirectorySync) {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        self.file.sync_all()
    }

    pub fn replace(&self, from: &str, to: &str) -> io::Result<()> {
        #[cfg(test)]
        if crate::direct::faults::take(crate::direct::faults::Fault::Rename) {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        fs::rename(self.path.join(from), self.path.join(to))
    }

    pub fn remove(&self, name: &str) -> io::Result<()> {
        let _measurement = crate::io_metrics::measure(0, |c| &mut c.unlink);
        #[cfg(test)]
        if crate::direct::faults::take(crate::direct::faults::Fault::Unlink) {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        fs::remove_file(self.path.join(name))
    }

    pub fn archive_bytes(file: &File, offset: u64) -> io::Result<u64> {
        file.metadata()?
            .len()
            .checked_sub(offset)
            .and_then(|bytes| bytes.checked_next_multiple_of(crate::BLOCK_SIZE as u64))
            .ok_or_else(|| io::Error::other("invalid archive output bound"))
    }

    pub fn archive(&self, name: &str, file: &File, offset: u64) -> io::Result<PathBuf> {
        use std::io::Write;
        use std::os::unix::fs::FileExt;
        let reserved = Self::archive_bytes(file, offset)?;
        let archive = self.path.join("rejected");
        if !archive.exists() {
            fs::create_dir(&archive)?;
            self.sync()?;
        }
        let mut attempt = 0u64;
        let (path, mut output) = loop {
            let path = archive.join(format!("{name}-from-{offset:020}-{attempt}"));
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
        if reserved != 0 {
            crate::direct::preallocate(&output, 0, reserved)?;
        }
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
