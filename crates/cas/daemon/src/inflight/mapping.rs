use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::ptr::NonNull;

use super::invalid;

const SEALS: libc::c_int = libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;

pub(super) fn create(bytes: usize) -> io::Result<File> {
    // SAFETY: the name is terminated and flags are supported memfd flags.
    let fd = unsafe {
        libc::memfd_create(
            c"cas-inflight".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful memfd_create transfers a new owned fd to us.
    let file = unsafe { File::from_raw_fd(fd) };
    file.set_len(bytes as u64)?;
    // SAFETY: file owns fd and F_ADD_SEALS takes an integer bitmask.
    if unsafe { libc::fcntl(fd, libc::F_ADD_SEALS, SEALS) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

pub(super) struct Mapping {
    pointer: NonNull<u8>,
    bytes: usize,
    pub file: File,
}

// SAFETY: the mapping has a stable address, owns its fd and only exposes atomic
// fields. Carrier transactions additionally require exclusive Rust access.
unsafe impl Send for Mapping {}
// SAFETY: shared access is restricted to aligned atomic loads/stores. Unmapping
// requires exclusive ownership after the last reference to Mapping is gone.
unsafe impl Sync for Mapping {}

impl Mapping {
    pub fn open(file: File, bytes: usize) -> io::Result<Self> {
        // SAFETY: F_GET_SEALS has no third argument and file owns the fd.
        let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
        if seals < 0 || seals & SEALS != SEALS || file.metadata()?.len() != bytes as u64 {
            return Err(invalid("inflight fd size or seals differ"));
        }
        // SAFETY: size is bounded by Geometry, offset is zero, and validated
        // seals prevent truncation. We retain file for the entire mapping.
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let Some(pointer) = NonNull::new(raw.cast()) else {
            // SAFETY: mmap succeeded and this is its exact address and size.
            unsafe { libc::munmap(raw, bytes) };
            return Err(invalid("inflight mapping has a null address"));
        };
        Ok(Self {
            pointer,
            bytes,
            file,
        })
    }

    /// Caller chooses only layout structs whose fields are aligned atomics and
    /// whose every bit pattern is valid. This remains private to the carrier.
    pub unsafe fn at<T>(&self, offset: usize) -> &T {
        assert!(
            offset
                .checked_add(size_of::<T>())
                .is_some_and(|end| end <= self.bytes)
        );
        assert_eq!(offset % align_of::<T>(), 0);
        // SAFETY: the caller supplies a valid atomic layout; checked bounds and
        // alignment keep it inside this live, page-aligned mapping.
        unsafe { &*self.pointer.as_ptr().add(offset).cast::<T>() }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: Mapping uniquely owns this mmap region and no borrow survives
        // its destruction. The file is dropped after the region is unmapped.
        unsafe { libc::munmap(self.pointer.as_ptr().cast(), self.bytes) };
    }
}
