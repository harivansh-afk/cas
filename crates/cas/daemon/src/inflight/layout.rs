use std::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64};

pub(super) const PAGE: usize = 4096;
pub(super) const MAGIC: u64 = u64::from_le_bytes(*b"CASIFL03");
pub(super) const VERSION: u32 = 3;
pub(super) const REJECTED: u32 = 1;
pub(super) const EMPTY: u32 = 0;
pub(super) const PREPARED: u32 = 1;
pub(super) const ACTIVE: u32 = 2;
pub(super) const DISCOVERED: u32 = 3;

#[repr(C)]
pub(super) struct Queue {
    pub features: AtomicU64,
    pub version: AtomicU16,
    pub descriptors: AtomicU16,
    pub last_head: AtomicU16,
    pub used: AtomicU16,
}

#[repr(C)]
pub(super) struct Descriptor {
    pub inflight: AtomicU8,
    pub reserved: [AtomicU8; 5],
    pub next: AtomicU16,
    pub counter: AtomicU64,
}

#[repr(C)]
pub(super) struct Header {
    pub magic: AtomicU64,
    pub version: AtomicU32,
    pub bytes: AtomicU32,
    pub store: [AtomicU64; 2],
    pub image: [AtomicU64; 2],
    pub epoch: AtomicU64,
    pub attachment: AtomicU64,
    pub queues: AtomicU32,
    pub queue_size: AtomicU32,
    pub slot_size: AtomicU32,
    pub failed: AtomicU32,
    pub serial: AtomicU64,
    pub mutation: AtomicU64,
    pub published: AtomicU64,
    pub available: [AtomicU32; 4],
    pub discovery: AtomicU64,
    pub reserved: [AtomicU8; PAGE - 128],
}

#[repr(C, align(64))]
pub(super) struct Slot {
    pub state: AtomicU32,
    pub kind: AtomicU16,
    pub queue: AtomicU16,
    pub head: AtomicU16,
    pub available: AtomicU16,
    pub flags: AtomicU32,
    pub serial: AtomicU64,
    pub mutation: AtomicU64,
    pub boundary: AtomicU64,
    pub offset: AtomicU64,
    pub length: AtomicU64,
    pub attachment: AtomicU64,
    pub discovery: AtomicU64,
    pub tail: [AtomicU8; 56],
}

const _: () = {
    use std::mem::{offset_of, size_of};
    assert!(size_of::<Queue>() == 16);
    assert!(size_of::<Descriptor>() == 16);
    assert!(offset_of!(Descriptor, counter) == 8);
    assert!(size_of::<Header>() == PAGE);
    assert!(offset_of!(Header, failed) == 76);
    assert!(offset_of!(Header, published) == 96);
    assert!(offset_of!(Header, available) == 104);
    assert!(size_of::<Slot>() == 128);
    assert!(offset_of!(Slot, serial) == 16);
    assert!(offset_of!(Slot, attachment) == 56);
};
