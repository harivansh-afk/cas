// Linux direct IO and file exclusion
// No buffered IO

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;

#[cfg(test)]
use crate::BLOCK_SIZE;
use crate::aligned::AlignedBuffer;

pub(crate) fn open(path: &Path, create: bool) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(create)
        .custom_flags(libc::O_DIRECT | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("staging log must be a regular file"));
    }
    file.try_lock().map_err(io::Error::from)?;
    Ok(file)
}

pub(crate) fn read(file: &File, buffer: &mut AlignedBuffer, offset: u64) -> io::Result<()> {
    let read = loop {
        match file.read_at(buffer.as_mut_slice(), offset) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => break result?,
        }
    };
    if read != buffer.as_slice().len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "short direct read",
        ));
    }
    Ok(())
}

pub(crate) fn write(file: &File, buffer: &AlignedBuffer, offset: u64) -> io::Result<()> {
    #[cfg(test)]
    if faults::take(faults::Fault::ShortWrite) {
        // Persist one aligned block, then report the short write through
        // same length check as the real syscall result.
        let written = file.write_at(&buffer.as_slice()[..BLOCK_SIZE], offset)?;
        return check_write_length(written, buffer.as_slice().len());
    }
    let written = loop {
        match file.write_at(buffer.as_slice(), offset) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => break result?,
        }
    };
    // A short direct write may leave a partial record. Never retry through a
    // potentially unaligned suffix; the caller poisons the writer on error.
    check_write_length(written, buffer.as_slice().len())
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
// This module and both injection sites are absent from production builds.
#[cfg(test)]
pub(crate) mod faults {
    use std::cell::Cell;

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Fault {
        ShortWrite,
        Sync,
    }

    thread_local! {
        static NEXT: Cell<Option<Fault>> = const { Cell::new(None) };
    }

    pub(crate) fn inject(fault: Fault) {
        NEXT.with(|next| assert!(next.replace(Some(fault)).is_none()));
    }

    pub(super) fn take(fault: Fault) -> bool {
        NEXT.with(|next| {
            if next.get() == Some(fault) {
                next.set(None);
                true
            } else {
                false
            }
        })
    }
}
