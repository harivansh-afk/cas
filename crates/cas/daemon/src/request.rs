// Decode the virtio wire format once
// Execution receives validated operations

use std::io;

use cas_core::{BLOCK_SIZE, MAX_REQUEST_BYTES};
use virtio_bindings::bindings::virtio_blk::{
    VIRTIO_BLK_T_FLUSH, VIRTIO_BLK_T_GET_ID, VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT,
};
use vm_memory::{Address, Bytes, GuestAddress, GuestMemory, GuestMemoryMmap, Permissions};

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
        let last = descriptors.iter().rfind(|segment| segment.len != 0)?;
        let status = last.addr.checked_add(last.len.checked_sub(1)? as u64)?;
        (last.writable && mem.check_range(status, 1, Permissions::Write))
            .then_some(Self { head, status })
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
    mut descriptors: Vec<Segment>,
    capacity_bytes: u64,
) -> io::Result<Request> {
    descriptors.retain(|segment| segment.len != 0);
    let completion = Completion::from_descriptors(mem, head, &descriptors)
        .ok_or_else(|| invalid("missing writable status byte"))?;
    let mut writable_seen = false;
    let mut total_len = 0usize;
    for segment in &descriptors {
        if writable_seen && !segment.writable {
            return Err(invalid("readable descriptor after writable descriptor"));
        }
        writable_seen |= segment.writable;
        let access = if segment.writable {
            Permissions::Write
        } else {
            Permissions::Read
        };
        if !mem.check_range(segment.addr, segment.len, access) {
            return Err(invalid("descriptor outside guest memory"));
        }
        total_len = total_len
            .checked_add(segment.len)
            .ok_or_else(|| invalid("length overflow"))?;
    }
    let len = total_len
        .checked_sub(16 + 1)
        .ok_or_else(|| invalid("missing request header"))?;
    if len > MAX_REQUEST_BYTES {
        return Err(invalid("request exceeds size limit"));
    }

    // Framing is in bytes, not descriptor boundaries. Work on the bounded
    // descriptor snapshot: pending IO must not re-read a guest descriptor table.
    let mut header = [0; 16];
    let mut copied = 0;
    for segment in &mut descriptors {
        if copied == header.len() {
            break;
        }
        if segment.writable {
            return Err(invalid("request header is not entirely readable"));
        }
        let count = segment.len.min(header.len() - copied);
        mem.read_slice(&mut header[copied..copied + count], segment.addr)
            .map_err(io::Error::other)?;
        segment.len -= count;
        if segment.len != 0 {
            segment.addr = segment
                .addr
                .checked_add(count as u64)
                .ok_or_else(|| invalid("address overflow"))?;
        }
        copied += count;
    }
    if copied != header.len() {
        return Err(invalid("missing request header"));
    }
    // Completion::from_descriptors established a nonempty final status buffer.
    descriptors.last_mut().unwrap().len -= 1;
    descriptors.retain(|segment| segment.len != 0);
    // These slices have fixed lengths within the local 16-byte header.
    let kind = u32::from_le_bytes(header[..4].try_into().unwrap());
    let sector = u64::from_le_bytes(header[8..].try_into().unwrap());
    let segments = descriptors;
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

    #[test]
    fn write_header_can_split_at_every_byte_and_share_the_payload_descriptor() {
        let (mem, original) = fixture(OUT, 8);
        let payload = vec![0x5a; BLOCK_SIZE];
        mem.write_slice(&payload, GuestAddress(16)).unwrap();
        for split in 0..=16 {
            let mut descriptors = Vec::new();
            if split != 0 {
                descriptors.push(Segment {
                    addr: GuestAddress(0),
                    len: split,
                    writable: false,
                });
            }
            descriptors.push(Segment {
                addr: GuestAddress(split as u64),
                len: 16 + BLOCK_SIZE - split,
                writable: false,
            });
            descriptors.push(*original.last().unwrap());
            let Request::Write(request) = parse(&mem, 7, descriptors, 8192).unwrap() else {
                panic!("expected write");
            };
            assert_eq!(request.offset, 4096);
            assert_eq!(request.len, BLOCK_SIZE);
            assert_eq!(request.segments.len(), 1);
            assert_eq!(request.segments[0].addr, GuestAddress(16));
            assert_eq!(request.segments[0].len, BLOCK_SIZE);
            assert_eq!(request.completion.status, GuestAddress(0xf000));
            let mut actual = vec![0; BLOCK_SIZE];
            mem.read_slice(&mut actual, request.segments[0].addr)
                .unwrap();
            assert_eq!(actual, payload);
        }
    }

    #[test]
    fn read_status_is_the_last_byte_of_scattered_writable_data() {
        let (mem, original) = fixture(IN, 0);
        let descriptors = vec![
            original[0],
            Segment {
                addr: GuestAddress(0x1000),
                len: 1024,
                writable: true,
            },
            Segment {
                addr: GuestAddress(0x3000),
                len: 3073,
                writable: true,
            },
        ];
        let Request::Read(request) = parse(&mem, 0, descriptors, 8192).unwrap() else {
            panic!("expected read");
        };
        assert_eq!(request.len, BLOCK_SIZE);
        assert_eq!(request.segments[0].len, 1024);
        assert_eq!(request.segments[1].len, 3072);
        assert_eq!(request.completion.status, GuestAddress(0x3c00));
    }

    #[test]
    fn get_id_accepts_a_shared_status_descriptor() {
        let (mem, original) = fixture(VIRTIO_BLK_T_GET_ID, 0);
        let descriptors = vec![
            original[0],
            Segment {
                addr: GuestAddress(0x1000),
                len: DEVICE_ID.len() + 1,
                writable: true,
            },
        ];
        let Request::GetId {
            completion,
            segments,
        } = parse(&mem, 0, descriptors, 8192).unwrap()
        else {
            panic!("expected GET_ID");
        };
        assert_eq!(completion.status, GuestAddress(0x1014));
        assert_eq!(segments[0].len, DEVICE_ID.len());
    }

    #[test]
    fn empty_descriptors_do_not_change_the_request_bytes() {
        let (mem, mut descriptors) = fixture(FLUSH, 0);
        for position in [2, 1, 0] {
            descriptors.insert(
                position,
                Segment {
                    addr: GuestAddress(u64::MAX),
                    len: 0,
                    writable: false,
                },
            );
        }
        assert!(matches!(
            parse(&mem, 0, descriptors, 8192).unwrap(),
            Request::Flush(_)
        ));
    }

    #[test]
    fn a_short_readable_header_cannot_borrow_bytes_from_writable_data() {
        let (mem, mut descriptors) = fixture(IN, 0);
        descriptors[0].len = 8;
        assert!(parse(&mem, 0, descriptors, 8192).is_err());
    }

    #[test]
    fn status_address_overflow_and_out_of_bounds_are_rejected() {
        let (mem, original) = fixture(IN, 0);
        for addr in [u64::MAX, 0xffff] {
            let mut descriptors = original.clone();
            *descriptors.last_mut().unwrap() = Segment {
                addr: GuestAddress(addr),
                len: 2,
                writable: true,
            };
            assert!(Completion::from_descriptors(&mem, 0, &descriptors).is_none());
            assert!(parse(&mem, 0, descriptors, 8192).is_err());
        }
    }
}
