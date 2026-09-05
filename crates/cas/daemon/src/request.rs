//! Decode the virtio wire format once; execution receives validated operations.
use std::io;

use cas_core::{BLOCK_SIZE, MAX_REQUEST_BYTES};
use virtio_bindings::bindings::virtio_blk::{
    VIRTIO_BLK_T_FLUSH, VIRTIO_BLK_T_GET_ID, VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT,
};
use vm_memory::{Bytes, GuestAddress, GuestMemory, GuestMemoryMmap, Permissions};

// Virtio addresses use 512-byte sectors even for a 4096-byte logical device.
pub(super) const SECTOR_BYTES: u64 = 512;
pub(super) const DEVICE_ID: &[u8; 20] = b"cas-experiment\0\0\0\0\0\0";

#[derive(Clone, Copy)]
pub(super) struct Segment {
    pub addr: GuestAddress,
    pub len: usize,
    pub writable: bool,
}

#[derive(Clone, Copy)]
pub(super) struct Completion {
    pub head: u16,
    pub status: GuestAddress,
}

impl Completion {
    pub fn from_descriptors(
        mem: &GuestMemoryMmap,
        head: u16,
        descriptors: &[Segment],
    ) -> Option<Self> {
        descriptors
            .last()
            .filter(|s| s.writable && s.len == 1 && mem.check_range(s.addr, 1, Permissions::Write))
            .map(|s| Self {
                head,
                status: s.addr,
            })
    }
}

pub(super) struct DataRequest {
    pub completion: Completion,
    pub offset: u64,
    pub segments: Vec<Segment>,
    pub len: usize,
}

pub(super) enum Request {
    Read(DataRequest),
    Write(DataRequest),
    Flush(Completion),
    GetId {
        completion: Completion,
        segments: Vec<Segment>,
    },
    Unsupported(Completion),
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum Status {
    Ok = virtio_bindings::bindings::virtio_blk::VIRTIO_BLK_S_OK as u8,
    IoError = virtio_bindings::bindings::virtio_blk::VIRTIO_BLK_S_IOERR as u8,
    Unsupported = virtio_bindings::bindings::virtio_blk::VIRTIO_BLK_S_UNSUPP as u8,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(super) fn parse(
    mem: &GuestMemoryMmap,
    head: u16,
    descriptors: Vec<Segment>,
    capacity_bytes: u64,
) -> io::Result<Request> {
    let completion = Completion::from_descriptors(mem, head, &descriptors)
        .ok_or_else(|| invalid("missing writable status byte"))?;
    if descriptors.len() < 2 {
        return Err(invalid("missing request header"));
    }
    let first = descriptors[0];
    if first.writable || first.len != 16 {
        return Err(invalid("invalid header descriptor"));
    }
    for segment in &descriptors {
        let access = if segment.writable {
            Permissions::Write
        } else {
            Permissions::Read
        };
        if segment.len == 0 || !mem.check_range(segment.addr, segment.len, access) {
            return Err(invalid("descriptor outside guest memory"));
        }
    }
    let mut header = [0; 16];
    mem.read_slice(&mut header, first.addr)
        .map_err(io::Error::other)?;
    // These slices have fixed lengths within the local 16-byte header.
    let kind = u32::from_le_bytes(header[..4].try_into().unwrap());
    let sector = u64::from_le_bytes(header[8..].try_into().unwrap());
    let mut segments = descriptors;
    segments.pop();
    segments.remove(0);
    let len = segments
        .iter()
        .try_fold(0usize, |n, s| n.checked_add(s.len))
        .ok_or_else(|| invalid("length overflow"))?;
    if len > MAX_REQUEST_BYTES {
        return Err(invalid("request exceeds size limit"));
    }
    match kind {
        VIRTIO_BLK_T_IN | VIRTIO_BLK_T_OUT => {
            let offset = sector
                .checked_mul(SECTOR_BYTES)
                .ok_or_else(|| invalid("sector overflow"))?;
            if len == 0
                || !len.is_multiple_of(BLOCK_SIZE)
                || !offset.is_multiple_of(BLOCK_SIZE as u64)
                || offset
                    .checked_add(len as u64)
                    .is_none_or(|end| end > capacity_bytes)
                || segments
                    .iter()
                    .any(|s| s.writable != (kind == VIRTIO_BLK_T_IN))
            {
                return Err(invalid("invalid data range, alignment, or direction"));
            }
            let data = DataRequest {
                completion,
                offset,
                segments,
                len,
            };
            Ok(if kind == VIRTIO_BLK_T_IN {
                Request::Read(data)
            } else {
                Request::Write(data)
            })
        }
        VIRTIO_BLK_T_FLUSH if len == 0 => Ok(Request::Flush(completion)),
        VIRTIO_BLK_T_FLUSH => Err(invalid("FLUSH has data descriptors")),
        VIRTIO_BLK_T_GET_ID if len == DEVICE_ID.len() && segments.iter().all(|s| s.writable) => {
            Ok(Request::GetId {
                completion,
                segments,
            })
        }
        VIRTIO_BLK_T_GET_ID => Err(invalid("invalid GET_ID buffer")),
        _ => Ok(Request::Unsupported(completion)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use virtio_bindings::bindings::virtio_blk::{
        VIRTIO_BLK_T_FLUSH as FLUSH, VIRTIO_BLK_T_IN as IN, VIRTIO_BLK_T_OUT as OUT,
    };

    fn fixture(kind: u32, sector: u64) -> (GuestMemoryMmap, Vec<Segment>) {
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let mut header = [0; 16];
        header[..4].copy_from_slice(&kind.to_le_bytes());
        header[8..].copy_from_slice(&sector.to_le_bytes());
        mem.write_slice(&header, GuestAddress(0)).unwrap();
        let mut descriptors = vec![Segment {
            addr: GuestAddress(0),
            len: 16,
            writable: false,
        }];
        if kind != FLUSH {
            descriptors.push(Segment {
                addr: GuestAddress(0x1000),
                len: BLOCK_SIZE,
                writable: kind == IN,
            });
        }
        descriptors.push(Segment {
            addr: GuestAddress(0xf000),
            len: 1,
            writable: true,
        });
        (mem, descriptors)
    }

    #[test]
    fn sector_units_remain_512_bytes() {
        let (mem, descriptors) = fixture(OUT, 8);
        let Request::Write(r) = parse(&mem, 7, descriptors, 8192).unwrap() else {
            panic!("expected write");
        };
        assert_eq!(r.offset, 4096);
        assert_eq!(r.len, 4096);
        assert_eq!(r.completion.head, 7);
    }

    #[test]
    fn rejects_misalignment_overflow_and_out_of_bounds() {
        for sector in [1, 16, u64::MAX] {
            let (mem, descriptors) = fixture(IN, sector);
            assert!(parse(&mem, 0, descriptors.clone(), 8192).is_err());
        }
        let (mem, mut descriptors) = fixture(IN, 0);
        descriptors[1].len = BLOCK_SIZE - 1;
        assert!(parse(&mem, 0, descriptors.clone(), 8192).is_err());
        descriptors[1].len = BLOCK_SIZE;
        descriptors[1].addr = GuestAddress(0xffff);
        assert!(parse(&mem, 0, descriptors.clone(), 8192).is_err());
    }

    #[test]
    fn scatter_gather_checks_direction_and_total_length() {
        let (mem, mut descriptors) = fixture(IN, 0);
        descriptors[1].len = 2048;
        descriptors.insert(
            2,
            Segment {
                addr: GuestAddress(0x3000),
                len: 2048,
                writable: true,
            },
        );
        assert!(matches!(
            parse(&mem, 0, descriptors.clone(), 8192).unwrap(),
            Request::Read(DataRequest {
                len: BLOCK_SIZE,
                ..
            })
        ));
        descriptors[2].writable = false;
        assert!(parse(&mem, 0, descriptors.clone(), 8192).is_err());
    }

    #[test]
    fn requires_header_and_writable_status() {
        let (mem, mut descriptors) = fixture(OUT, 0);
        assert!(parse(&mem, 0, descriptors[..1].to_vec(), 8192).is_err());
        descriptors[0].writable = true;
        assert!(parse(&mem, 0, descriptors.clone(), 8192).is_err());
        descriptors[0].writable = false;
        descriptors[2].writable = false;
        assert!(parse(&mem, 0, descriptors.clone(), 8192).is_err());
    }

    #[test]
    fn flush_has_no_data_payload() {
        let (mem, mut descriptors) = fixture(FLUSH, 0);
        assert!(matches!(
            parse(&mem, 0, descriptors.clone(), 8192).unwrap(),
            Request::Flush(_)
        ));
        descriptors.insert(
            1,
            Segment {
                addr: GuestAddress(0x1000),
                len: BLOCK_SIZE,
                writable: false,
            },
        );
        assert!(parse(&mem, 0, descriptors.clone(), 8192).is_err());
    }
}
