//! One fixed-size FIEMAP result, including allocated unwritten extents beyond
//! EOF. Layout follows Linux's include/uapi/linux/fiemap.h; no block-device IO.
use crate::{BLOCK_SIZE, encoding::require};
use std::{fs::File, io, ops::Range, os::fd::AsRawFd};

#[derive(Default)]
#[repr(C)]
struct Header {
    start: u64,
    length: u64,
    flags: u32,
    mapped: u32,
    count: u32,
    reserved: u32,
}

#[derive(Default)]
#[repr(C)]
struct Extent {
    logical: u64,
    physical: u64,
    length: u64,
    reserved64: [u64; 2],
    flags: u32,
    reserved: [u32; 3],
}

#[derive(Default)]
#[repr(C)]
struct Map {
    header: Header,
    extent: Extent,
}

const _: () =
    assert!(size_of::<Header>() == 32 && size_of::<Extent>() == 56 && size_of::<Map>() == 88);
const REQUEST: libc::Ioctl = libc::_IOWR::<Header>(b'f' as u32, 11);
const SUPPORTED: u32 = 0x0001 | 0x0800 | 0x2000; // LAST, UNWRITTEN, SHARED.

/// The caller has synced the file and excludes changes while enumerating.
/// Returns the first mapped logical range at/after start, clamped to start.
pub(crate) fn next_extent(file: &File, start: u64) -> io::Result<Option<Range<u64>>> {
    require(
        start <= i64::MAX as u64 && start.is_multiple_of(BLOCK_SIZE as u64),
        "invalid FIEMAP start",
    )?;
    #[cfg(test)]
    if super::faults::take(super::faults::Fault::Map) {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    let mut map = Map {
        header: Header {
            start,
            length: u64::MAX - start,
            count: 1,
            ..Header::default()
        },
        ..Map::default()
    };
    // SAFETY: Map contains the exact UAPI header followed by one writable extent,
    // as requested by count=1. The FD and buffer stay live until ioctl returns;
    // this ioctl retains no userspace pointer or asynchronous operation.
    if unsafe { libc::ioctl(file.as_raw_fd(), REQUEST, &mut map) } != 0 {
        return Err(io::Error::last_os_error());
    }
    require(
        map.header.flags == 0 && map.header.mapped <= 1,
        "invalid FIEMAP response",
    )?;
    if map.header.mapped == 0 {
        return Ok(None);
    }
    let extent = map.extent;
    let end = extent
        .logical
        .checked_add(extent.length)
        .filter(|&end| end <= i64::MAX as u64 && end > start)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid FIEMAP extent end"))?;
    require(
        extent.length != 0
            && extent.flags & !SUPPORTED == 0
            && extent.logical.is_multiple_of(BLOCK_SIZE as u64)
            && extent.length.is_multiple_of(BLOCK_SIZE as u64),
        "unsupported FIEMAP extent",
    )?;
    Ok(Some(extent.logical.max(start)..end))
}
