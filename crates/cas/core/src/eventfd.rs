//! Nonblocking readiness signals shared by cache fills and IO scheduling.
use std::{
    io,
    os::fd::{AsRawFd, BorrowedFd, OwnedFd},
};

/// Duplicate a live eventfd (or any descriptor owner) into an owned handle,
/// for a waiter or transport that signals the owner's wake. The duplicate
/// shares the counter; closing it never closes the original. The owner is
/// borrowed for the call, so the descriptor cannot be closed underneath it.
pub fn duplicate(owner: &impl AsRawFd) -> io::Result<OwnedFd> {
    // SAFETY: `owner` is borrowed for the whole call and owns the descriptor
    // it reports, so the raw fd is open and the borrow does not outlive it.
    unsafe { BorrowedFd::borrow_raw(owner.as_raw_fd()) }.try_clone_to_owned()
}

pub fn notify(fd: BorrowedFd<'_>) -> io::Result<()> {
    let one = 1u64;
    loop {
        // SAFETY: the borrowed FD remains live and one is an initialized u64.
        let written = unsafe {
            libc::write(
                fd.as_raw_fd(),
                (&one as *const u64).cast(),
                size_of::<u64>(),
            )
        };
        if written == size_of::<u64>() as isize {
            return Ok(());
        }
        if written >= 0 {
            return Err(io::Error::other("short eventfd notification"));
        }
        let error = io::Error::last_os_error();
        match error.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => return Ok(()), // Readiness is already pending.
            _ => return Err(error),
        }
    }
}
