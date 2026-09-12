// Linux direct IO and file exclusion
// No buffered IO

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;

use crate::BLOCK_SIZE;
use crate::aligned::AlignedBuffer;

mod fiemap;
pub(crate) use fiemap::next_extent;

pub(crate) fn truncate(file: &File, length: u64) -> io::Result<()> {
    crate::encoding::require(
        length <= i64::MAX as u64 && length.is_multiple_of(BLOCK_SIZE as u64),
        "invalid truncate length",
    )?;
    #[cfg(test)]
    if faults::take(faults::Fault::Truncate) {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    file.set_len(length)
}

pub(crate) fn open(path: &Path, create: bool) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(create)
        .custom_flags(libc::O_DIRECT | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("storage IO requires a regular file"));
    }
    file.try_lock().map_err(io::Error::from)?;
    Ok(file)
}

/// Remove aligned payload extents without changing file length or framing.
pub(crate) fn punch(file: &File, offset: u64, length: u64) -> io::Result<()> {
    #[cfg(test)]
    if faults::take(faults::Fault::Punch) {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    fallocate(
        file,
        offset,
        length,
        libc::FALLOC_FL_KEEP_SIZE | libc::FALLOC_FL_PUNCH_HOLE,
    )
}

pub(crate) fn sync_all(file: &File) -> io::Result<()> {
    #[cfg(test)]
    if faults::take(faults::Fault::FileSync) {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    file.sync_all()
}

/// Require actual filesystem reflink; no ordinary-copy fallback is permitted.
pub(crate) fn reflink(source: &File, destination: &File) -> io::Result<()> {
    #[cfg(test)]
    if faults::take(faults::Fault::Reflink) {
        return Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP));
    }
    // SAFETY: both file descriptions remain open. FICLONE takes a source FD
    // as its integer argument and retains no userspace pointer after return.
    if unsafe { libc::ioctl(destination.as_raw_fd(), libc::FICLONE, source.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn preallocate(file: &File, offset: u64, length: u64) -> io::Result<()> {
    #[cfg(test)]
    if faults::take(faults::Fault::Allocate) {
        return Err(io::Error::from_raw_os_error(libc::ENOSPC));
    }
    // KEEP_SIZE reserves extents without making unwritten records visible at EOF.
    fallocate(file, offset, length, libc::FALLOC_FL_KEEP_SIZE)
}

fn fallocate(file: &File, offset: u64, length: u64, mode: i32) -> io::Result<()> {
    if length == 0
        || !offset.is_multiple_of(BLOCK_SIZE as u64)
        || !length.is_multiple_of(BLOCK_SIZE as u64)
        || offset
            .checked_add(length)
            .is_none_or(|end| end > i64::MAX as u64)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid allocation range",
        ));
    }
    // SAFETY: a live descriptor and a checked aligned positive off_t range.
    if unsafe { libc::fallocate(file.as_raw_fd(), mode, offset as i64, length as i64) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn read(file: &File, buffer: &mut AlignedBuffer, offset: u64) -> io::Result<()> {
    read_bytes(file, buffer.as_mut_slice(), offset)
}

pub(crate) fn read_bytes(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<()> {
    aligned(buffer, offset)?;
    #[cfg(test)]
    if faults::take(faults::Fault::Read) {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    let read = loop {
        match file.read_at(buffer, offset) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => break result?,
        }
    };
    if read != buffer.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "short direct read",
        ));
    }
    Ok(())
}

pub(crate) fn write(file: &File, buffer: &AlignedBuffer, offset: u64) -> io::Result<()> {
    write_bytes(file, buffer.as_slice(), offset)
}

pub(crate) fn write_bytes(file: &File, buffer: &[u8], offset: u64) -> io::Result<()> {
    aligned(buffer, offset)?;
    #[cfg(test)]
    if faults::take(faults::Fault::Write) {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    #[cfg(test)]
    if faults::take(faults::Fault::ShortWrite) {
        // Persist one aligned block, then report the short write through
        // same length check as the real syscall result.
        let written = file.write_at(&buffer[..BLOCK_SIZE], offset)?;
        return check_write_length(written, buffer.len());
    }
    let written = loop {
        match file.write_at(buffer, offset) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => break result?,
        }
    };
    // A short direct write may leave a partial record. Never retry through a
    // potentially unaligned suffix; the caller poisons the writer on error.
    check_write_length(written, buffer.len())
}

fn aligned(buffer: &[u8], offset: u64) -> io::Result<()> {
    if buffer.is_empty()
        || !(buffer.as_ptr() as usize).is_multiple_of(BLOCK_SIZE)
        || !buffer.len().is_multiple_of(BLOCK_SIZE)
        || !offset.is_multiple_of(BLOCK_SIZE as u64)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unaligned direct IO",
        ));
    }
    Ok(())
}

/// Alignment requirements reported by this backing file's filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Alignment {
    pub memory: u32,
    pub offset: u32,
}

impl Alignment {
    pub fn query(file: &File) -> io::Result<Self> {
        let mut stat = std::mem::MaybeUninit::<libc::statx>::zeroed();
        // SAFETY: the live file descriptor and empty NUL-terminated path select
        // the file itself; stat points to writable storage of the required size.
        let result = unsafe {
            libc::statx(
                file.as_raw_fd(),
                c"".as_ptr(),
                libc::AT_EMPTY_PATH,
                libc::STATX_DIOALIGN,
                stat.as_mut_ptr(),
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a successful statx initialized this zeroed structure.
        let stat = unsafe { stat.assume_init() };
        if stat.stx_mask & libc::STATX_DIOALIGN == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "backing file does not report direct IO alignment",
            ));
        }
        let alignment = Self {
            memory: stat.stx_dio_mem_align,
            offset: stat.stx_dio_offset_align,
        };
        alignment.validate()?;
        Ok(alignment)
    }

    fn validate(self) -> io::Result<()> {
        if [self.memory, self.offset]
            .into_iter()
            .any(|value| value == 0 || !(BLOCK_SIZE as u32).is_multiple_of(value))
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "backing file requires unsupported direct IO alignment",
            ));
        }
        Ok(())
    }
}

fn check_write_length(written: usize, expected: usize) -> io::Result<()> {
    if written != expected {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "short direct write",
        ));
    }
    Ok(())
}

pub(crate) fn sync_data(file: &File) -> io::Result<()> {
    #[cfg(test)]
    if faults::take(faults::Fault::Sync) {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    file.sync_data()
}

// One-shot, thread-local syscall faults keep concurrent tests independent.
// This module and its injection sites are absent from production builds.
#[cfg(test)]
pub(crate) mod faults {
    use std::{
        cell::{Cell, RefCell},
        sync::mpsc,
    };

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Fault {
        Truncate,
        Map,
        Rename,
        Reflink,
        Write,
        FileSync,
        DirectorySync,
        Allocate,
        Punch,
        Read,
        ShortWrite,
        Sync,
    }

    thread_local! {
        static NEXT: Cell<Option<(Fault, usize)>> = const { Cell::new(None) };
        static PAUSE: RefCell<Option<Pause>> = const { RefCell::new(None) };
    }

    struct Pause {
        fault: Fault,
        entered: mpsc::Sender<()>,
        resume: mpsc::Receiver<()>,
    }

    /// Stop at the actual syscall boundary, independently of one-shot failure.
    pub(crate) fn pause_before(
        fault: Fault,
        entered: mpsc::Sender<()>,
        resume: mpsc::Receiver<()>,
    ) {
        PAUSE.with(|pause| {
            assert!(
                pause
                    .replace(Some(Pause {
                        fault,
                        entered,
                        resume
                    }))
                    .is_none()
            )
        });
    }

    pub(crate) fn inject(fault: Fault) {
        inject_after(fault, 0);
    }

    pub(crate) fn inject_after(fault: Fault, successful_calls: usize) {
        NEXT.with(|next| assert!(next.replace(Some((fault, successful_calls))).is_none()));
    }

    pub(crate) fn take(fault: Fault) -> bool {
        let pause = PAUSE.with(|pause| {
            let mut pause = pause.borrow_mut();
            if pause.as_ref().is_some_and(|pause| pause.fault == fault) {
                pause.take()
            } else {
                None
            }
        });
        if let Some(pause) = pause {
            pause.entered.send(()).expect("pause observer");
            pause
                .resume
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("pause release");
        }
        NEXT.with(|next| match next.get() {
            Some((expected, 0)) if expected == fault => {
                next.set(None);
                true
            }
            Some((expected, remaining)) if expected == fault => {
                next.set(Some((expected, remaining - 1)));
                false
            }
            _ => false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alignment_is_explicit_and_invalid_buffers_fail_before_io() {
        for (memory, offset, accepted) in [
            (512, 512, true),
            (4096, 4096, true),
            (0, 4096, false),
            (8192, 4096, false),
            (4096, 8192, false),
            (3, 512, false),
        ] {
            assert_eq!(Alignment { memory, offset }.validate().is_ok(), accepted);
        }
        let directory = tempfile::tempdir().unwrap();
        let file = open(&directory.path().join("direct"), true).unwrap();
        let alignment = Alignment::query(&file).unwrap();
        eprintln!("backing alignment: {alignment:?}");
        let mut buffer = AlignedBuffer::new(2 * BLOCK_SIZE);
        assert!(write_bytes(&file, &buffer.as_slice()[1..], 0).is_err());
        assert!(write_bytes(&file, buffer.as_slice(), 512).is_err());
        write_bytes(&file, &buffer.as_slice()[..BLOCK_SIZE], 0).unwrap();
        read_bytes(&file, &mut buffer.as_mut_slice()[..BLOCK_SIZE], 0).unwrap();
    }
}
