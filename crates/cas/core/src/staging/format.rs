// Fixed-size, little-endian staging format

use super::RECORD_SIZE;
use crate::{
    BLOCK_SIZE,
    aligned::AlignedBuffer,
    encoding::{put64, u64_at},
};

const CHECKSUM_OFFSET: usize = BLOCK_SIZE - 4;
const FILE_MAGIC: &[u8; 8] = b"CASLOG01";
const RECORD_MAGIC: &[u8; 8] = b"CASREC01";

const KIND_OFFSET: usize = 8;
const SEQUENCE_OFFSET: usize = 16;
const LOGICAL_OFFSET: usize = 24;
const LENGTH_OFFSET: usize = 32;
const IMAGE_BYTES_OFFSET: usize = 8;
const RECORD_SIZE_OFFSET: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub(super) enum RecordKind {
    Write = 1,
    Zero = 2,
    Fence = 3,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Record {
    pub(super) kind: RecordKind,
    pub(super) sequence: u64,
    pub(super) offset: u64,
    pub(super) length: u64,
}

fn checksum(bytes: &[u8]) -> u32 {
    let mut crc = crc32fast::Hasher::new();
    crc.update(&bytes[..CHECKSUM_OFFSET]);
    crc.update(&bytes[BLOCK_SIZE..]);
    crc.finalize()
}

fn seal(bytes: &mut [u8]) {
    let crc = checksum(bytes);
    bytes[CHECKSUM_OFFSET..BLOCK_SIZE].copy_from_slice(&crc.to_le_bytes());
}

fn checksum_valid(bytes: &[u8]) -> bool {
    let stored = u32::from_le_bytes(bytes[CHECKSUM_OFFSET..BLOCK_SIZE].try_into().unwrap());
    checksum(bytes) == stored
}

pub(super) fn decode(bytes: &[u8]) -> Option<Record> {
    if &bytes[..8] != RECORD_MAGIC || !checksum_valid(bytes) {
        return None;
    }
    let record = Record {
        kind: match u64_at(bytes, KIND_OFFSET) {
            1 => RecordKind::Write,
            2 => RecordKind::Zero,
            3 => RecordKind::Fence,
            _ => return None,
        },
        sequence: u64_at(bytes, SEQUENCE_OFFSET),
        offset: u64_at(bytes, LOGICAL_OFFSET),
        length: u64_at(bytes, LENGTH_OFFSET),
    };
    match record.kind {
        RecordKind::Write if record.length == BLOCK_SIZE as u64 => Some(record),
        RecordKind::Zero if record.length > 0 && bytes[BLOCK_SIZE..].iter().all(|b| *b == 0) => {
            Some(record)
        }
        RecordKind::Fence
            if record.offset == 0
                && record.length == 0
                && bytes[BLOCK_SIZE..].iter().all(|b| *b == 0) =>
        {
            Some(record)
        }
        _ => None,
    }
}

pub(super) fn encode(record: Record, payload: &[u8]) -> AlignedBuffer {
    let mut buffer = AlignedBuffer::new(RECORD_SIZE);
    let bytes = buffer.as_mut_slice();
    bytes[..8].copy_from_slice(RECORD_MAGIC);
    put64(bytes, KIND_OFFSET, record.kind as u64);
    put64(bytes, SEQUENCE_OFFSET, record.sequence);
    put64(bytes, LOGICAL_OFFSET, record.offset);
    put64(bytes, LENGTH_OFFSET, record.length);
    bytes[BLOCK_SIZE..BLOCK_SIZE + payload.len()].copy_from_slice(payload);
    seal(bytes);
    buffer
}

pub(super) fn encode_header(image_bytes: u64) -> AlignedBuffer {
    let mut header = AlignedBuffer::new(BLOCK_SIZE);
    let bytes = header.as_mut_slice();
    bytes[..8].copy_from_slice(FILE_MAGIC);
    put64(bytes, IMAGE_BYTES_OFFSET, image_bytes);
    put64(bytes, RECORD_SIZE_OFFSET, RECORD_SIZE as u64);
    seal(bytes);
    header
}

pub(super) fn decode_header(bytes: &[u8]) -> Option<u64> {
    let image_bytes = u64_at(bytes, IMAGE_BYTES_OFFSET);
    (&bytes[..8] == FILE_MAGIC
        && checksum_valid(bytes)
        && u64_at(bytes, RECORD_SIZE_OFFSET) == RECORD_SIZE as u64
        && image_bytes > 0
        && image_bytes.is_multiple_of(BLOCK_SIZE as u64))
    .then_some(image_bytes)
}
