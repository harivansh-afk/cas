//! The only unsafe allocation code. No buffered-IO fallback.

use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;
use std::ptr::NonNull;

use crate::BLOCK_SIZE;

pub(crate) struct Aligned {
    pointer: NonNull<u8>,
    layout: Layout,
}

impl Aligned {
    pub(crate) fn new(length: usize) -> Self {
        assert!(length > 0 && length.is_multiple_of(BLOCK_SIZE));
        let layout = Layout::from_size_align(length, BLOCK_SIZE).unwrap();
        // SAFETY: layout has a positive size and a valid power-of-two alignment.
        let pointer = unsafe { alloc_zeroed(layout) };
        let pointer = NonNull::new(pointer).unwrap_or_else(|| handle_alloc_error(layout));
        Self { pointer, layout }
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        // SAFETY: allocation is initialized, live, and borrowed for this slice.
        unsafe { std::slice::from_raw_parts(self.pointer.as_ptr(), self.layout.size()) }
    }

    pub(crate) fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: &mut self guarantees exclusive access to the live allocation.
        unsafe { std::slice::from_raw_parts_mut(self.pointer.as_ptr(), self.layout.size()) }
    }
}

impl Drop for Aligned {
    fn drop(&mut self) {
        // SAFETY: pointer was allocated with this layout and is freed once.
        unsafe { dealloc(self.pointer.as_ptr(), self.layout) };
    }
}

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

pub(crate) fn read(file: &File, buffer: &mut Aligned, offset: u64) -> io::Result<()> {
    let read = loop {
        match file.read_at(buffer.bytes_mut(), offset) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => break result?,
        }
    };
    if read != buffer.bytes().len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "short direct read",
        ));
    }
    Ok(())
}

pub(crate) fn write(file: &File, buffer: &Aligned, offset: u64) -> io::Result<()> {
    let written = loop {
        match file.write_at(buffer.bytes(), offset) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => break result?,
        }
    };
    // A short direct write may leave a partial record. Never retry through a
    // potentially unaligned suffix; the caller poisons the writer on error.
    if written != buffer.bytes().len() {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "short direct write",
        ));
    }
    Ok(())
}
