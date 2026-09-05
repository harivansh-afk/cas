//! Synchronous, local-class staging reference. See docs/implementation.md for
//! the provisional format and the distinction between these tests and G1/G2.

use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::path::Path;

use crate::BLOCK_SIZE;
use crate::direct::{self, Aligned};

pub const RECORD_SIZE: usize = 2 * BLOCK_SIZE;
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const CHECKSUM_OFFSET: usize = BLOCK_SIZE - 4;
const FILE_MAGIC: &[u8; 8] = b"CASLOG01";
const RECORD_MAGIC: &[u8; 8] = b"CASREC01";
const WRITE: u64 = 1;
const ZERO: u64 = 2;
const FENCE: u64 = 3;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("invalid staging header")]
    Header,
    #[error("corrupt record in the committed prefix at file offset {0}")]
    Corrupt(u64),
    #[error("range must be within the image and aligned to 4096 bytes")]
    Range,
    #[error("read/write request exceeds 1 MiB")]
    RequestTooLarge,
    #[error("writer stopped after an IO failure; reopen to recover")]
    Poisoned,
    #[error("sequence number or file offset exhausted")]
    Exhausted,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub image_bytes: u64,
    pub appended: u64,
    pub durable: u64,
    pub log_bytes: u64,
    pub mapped_blocks: usize,
    pub recovered_tail_bytes: u64,
}

/// One exclusive writer per file. `&mut self` serializes sequence assignment,
/// append, and FLUSH; the async implementation must retain that ordering.
pub struct StagingLog {
    file: File,
    image_bytes: u64,
    next_offset: u64,
    appended: u64,
    durable: u64,
    blocks: BTreeMap<u64, u64>,
    poisoned: bool,
    recovered_tail_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct Record {
    kind: u64,
    sequence: u64,
    offset: u64,
    length: u64,
}

fn get_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
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

fn decode(bytes: &[u8]) -> Option<Record> {
    if &bytes[..8] != RECORD_MAGIC || !checksum_valid(bytes) {
        return None;
    }
    let record = Record {
        kind: get_u64(bytes, 8),
        sequence: get_u64(bytes, 16),
        offset: get_u64(bytes, 24),
        length: get_u64(bytes, 32),
    };
    match record.kind {
        WRITE if record.length == BLOCK_SIZE as u64 => Some(record),
        ZERO if record.length > 0 && bytes[BLOCK_SIZE..].iter().all(|b| *b == 0) => Some(record),
        FENCE
            if record.offset == 0
                && record.length == 0
                && bytes[BLOCK_SIZE..].iter().all(|b| *b == 0) =>
        {
            Some(record)
        }
        _ => None,
    }
}

fn encode(record: Record, payload: &[u8]) -> Aligned {
    let mut buffer = Aligned::new(RECORD_SIZE);
    let bytes = buffer.bytes_mut();
    bytes[..8].copy_from_slice(RECORD_MAGIC);
    put_u64(bytes, 8, record.kind);
    put_u64(bytes, 16, record.sequence);
    put_u64(bytes, 24, record.offset);
    put_u64(bytes, 32, record.length);
    bytes[BLOCK_SIZE..BLOCK_SIZE + payload.len()].copy_from_slice(payload);
    seal(bytes);
    buffer
}

impl StagingLog {
    /// Creates a zero-initialized logical image. Never replaces an existing log.
    /// This does not import a raw image or implement a compacted base manifest.
    pub fn create(path: impl AsRef<Path>, image_bytes: u64) -> Result<Self> {
        let path = path.as_ref();
        if image_bytes == 0 || !image_bytes.is_multiple_of(BLOCK_SIZE as u64) {
            return Err(Error::Range);
        }
        let file = direct::open(path, true)?;
        let mut header = Aligned::new(BLOCK_SIZE);
        header.bytes_mut()[..8].copy_from_slice(FILE_MAGIC);
        put_u64(header.bytes_mut(), 8, image_bytes);
        put_u64(header.bytes_mut(), 16, RECORD_SIZE as u64);
        seal(header.bytes_mut());
        direct::write(&file, &header, 0)?;
        file.sync_all()?;
        // A synced file does not by itself make a newly created name durable.
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        File::open(parent)?.sync_all()?;
        Ok(Self {
            file,
            image_bytes,
            next_offset: BLOCK_SIZE as u64,
            appended: 0,
            durable: 0,
            blocks: BTreeMap::new(),
            poisoned: false,
            recovered_tail_bytes: 0,
        })
    }

    /// Replays through the last complete, checksum-valid FLUSH fence. Later
    /// writes need not survive the volatile-write-cache contract and are cut.
    /// A valid fence after a damaged record is an error, never a shorter replay.
    /// Reopening mutates the uncommitted suffix and requires the writer lock.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let file = direct::open(path.as_ref(), false)?;
        let original_length = file.metadata()?.len();
        if original_length < BLOCK_SIZE as u64 {
            return Err(Error::Header);
        }
        let mut header = Aligned::new(BLOCK_SIZE);
        direct::read(&file, &mut header, 0)?;
        let image_bytes = get_u64(header.bytes(), 8);
        if &header.bytes()[..8] != FILE_MAGIC
            || !checksum_valid(header.bytes())
            || get_u64(header.bytes(), 16) != RECORD_SIZE as u64
            || image_bytes == 0
            || !image_bytes.is_multiple_of(BLOCK_SIZE as u64)
        {
            return Err(Error::Header);
        }
        let slots = (original_length - BLOCK_SIZE as u64) / RECORD_SIZE as u64;
        let mut committed_slots = 0;
        let mut buffer = Aligned::new(RECORD_SIZE);
        // Fixed slots prevent guest payloads resembling a fence from becoming
        // metadata. Reverse scan establishes the bound before validating replay.
        for slot in (0..slots).rev() {
            let position = BLOCK_SIZE as u64 + slot * RECORD_SIZE as u64;
            direct::read(&file, &mut buffer, position)?;
            if decode(buffer.bytes()).is_some_and(|record| record.kind == FENCE) {
                committed_slots = slot + 1;
                break;
            }
        }
        let mut log = Self {
            file,
            image_bytes,
            next_offset: BLOCK_SIZE as u64 + committed_slots * RECORD_SIZE as u64,
            appended: 0,
            durable: 0,
            blocks: BTreeMap::new(),
            poisoned: false,
            recovered_tail_bytes: 0,
        };
        for slot in 0..committed_slots {
            let position = BLOCK_SIZE as u64 + slot * RECORD_SIZE as u64;
            direct::read(&log.file, &mut buffer, position)?;
            let record = decode(buffer.bytes()).ok_or(Error::Corrupt(position))?;
            if record.kind == FENCE {
                if record.sequence != log.appended {
                    return Err(Error::Corrupt(position));
                }
                log.durable = record.sequence;
            } else {
                if log.appended.checked_add(1) != Some(record.sequence)
                    || log.validate_range(record.offset, record.length).is_err()
                {
                    return Err(Error::Corrupt(position));
                }
                log.apply(record, position);
                log.appended = record.sequence;
            }
        }
        log.recovered_tail_bytes = original_length - log.next_offset;
        log.file.set_len(log.next_offset)?;
        log.file.sync_all()?;
        Ok(log)
    }

    pub fn status(&self) -> Status {
        Status {
            image_bytes: self.image_bytes,
            appended: self.appended,
            durable: self.durable,
            log_bytes: self.next_offset,
            mapped_blocks: self.blocks.len(),
            recovered_tail_bytes: self.recovered_tail_bytes,
        }
    }

    fn healthy(&self) -> Result<()> {
        if self.poisoned {
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }

    fn validate_range(&self, offset: u64, length: u64) -> Result<()> {
        if !offset.is_multiple_of(BLOCK_SIZE as u64)
            || !length.is_multiple_of(BLOCK_SIZE as u64)
            || offset
                .checked_add(length)
                .is_none_or(|end| end > self.image_bytes)
        {
            return Err(Error::Range);
        }
        Ok(())
    }

    fn apply(&mut self, record: Record, position: u64) {
        if record.kind == WRITE {
            self.blocks.insert(record.offset, position);
        } else if record.kind == ZERO {
            let end = record.offset + record.length;
            self.blocks
                .retain(|offset, _| *offset < record.offset || *offset >= end);
        }
    }

    fn append(&mut self, record: Record, payload: &[u8]) -> Result<()> {
        let end = self
            .next_offset
            .checked_add(RECORD_SIZE as u64)
            .ok_or(Error::Exhausted)?;
        let buffer = encode(record, payload);
        if let Err(error) = direct::write(&self.file, &buffer, self.next_offset) {
            self.poisoned = true;
            return Err(error.into());
        }
        self.apply(record, self.next_offset);
        self.next_offset = end;
        if record.kind != FENCE {
            self.appended = record.sequence;
        }
        Ok(())
    }

    /// Returns the final append sequence, or None for an empty request.
    /// A multi-block request is ordered but has no multi-block atomicity promise.
    pub fn write(&mut self, offset: u64, bytes: &[u8]) -> Result<Option<u64>> {
        self.healthy()?;
        self.validate_range(offset, bytes.len() as u64)?;
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(Error::RequestTooLarge);
        }
        for (index, block) in bytes.chunks_exact(BLOCK_SIZE).enumerate() {
            let sequence = self.appended.checked_add(1).ok_or(Error::Exhausted)?;
            self.append(
                Record {
                    kind: WRITE,
                    sequence,
                    offset: offset + (index * BLOCK_SIZE) as u64,
                    length: BLOCK_SIZE as u64,
                },
                block,
            )?;
        }
        Ok((!bytes.is_empty()).then_some(self.appended))
    }

    /// DISCARD and WRITE_ZEROES share this range record. An empty discard must
    /// never reserve a sequence: no nonexistent append can hold up FLUSH.
    pub fn zero(&mut self, offset: u64, length: u64) -> Result<Option<u64>> {
        self.healthy()?;
        self.validate_range(offset, length)?;
        if length == 0 {
            return Ok(None);
        }
        let sequence = self.appended.checked_add(1).ok_or(Error::Exhausted)?;
        self.append(
            Record {
                kind: ZERO,
                sequence,
                offset,
                length,
            },
            &[],
        )?;
        Ok(Some(sequence))
    }

    /// The fence and preceding writes are covered by one fdatasync. E advances
    /// only after it succeeds. Dropping the log does not perform a FLUSH.
    pub fn flush(&mut self) -> Result<u64> {
        self.healthy()?;
        if self.appended != self.durable {
            self.append(
                Record {
                    kind: FENCE,
                    sequence: self.appended,
                    offset: 0,
                    length: 0,
                },
                &[],
            )?;
            if let Err(error) = self.file.sync_data() {
                self.poisoned = true;
                return Err(error.into());
            }
            self.durable = self.appended;
        }
        Ok(self.durable)
    }

    pub fn read(&self, offset: u64, length: usize) -> Result<Vec<u8>> {
        self.healthy()?;
        self.validate_range(offset, length as u64)?;
        if length > MAX_REQUEST_BYTES {
            return Err(Error::RequestTooLarge);
        }
        let mut result = vec![0; length];
        let mut buffer = Aligned::new(RECORD_SIZE);
        for (index, block) in result.chunks_exact_mut(BLOCK_SIZE).enumerate() {
            let logical_offset = offset + (index * BLOCK_SIZE) as u64;
            if let Some(&position) = self.blocks.get(&logical_offset) {
                direct::read(&self.file, &mut buffer, position)?;
                let record = decode(buffer.bytes()).ok_or(Error::Corrupt(position))?;
                if record.kind != WRITE || record.offset != logical_offset {
                    return Err(Error::Corrupt(position));
                }
                block.copy_from_slice(&buffer.bytes()[BLOCK_SIZE..]);
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_append_poisoning_prevents_a_later_flush_ack() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = directory.path().join("image.log");
        let mut log = StagingLog::create(&path, (4 * BLOCK_SIZE) as u64).unwrap();
        log.write(0, &vec![1; BLOCK_SIZE]).unwrap();
        log.flush().unwrap();
        let durable = log.status().durable;
        // Replace only this test object's descriptor with a read-only one to
        // cause a real EBADF write failure, without introducing production hooks.
        log.file = File::open(&path).unwrap();
        assert!(matches!(
            log.write(0, &vec![2; BLOCK_SIZE]),
            Err(Error::Io(_))
        ));
        assert!(matches!(log.flush(), Err(Error::Poisoned)));
        assert!(matches!(log.read(0, BLOCK_SIZE), Err(Error::Poisoned)));
        assert_eq!(log.status().durable, durable);
        drop(log);
        let log = StagingLog::open(path).unwrap();
        assert_eq!(log.read(0, BLOCK_SIZE).unwrap(), vec![1; BLOCK_SIZE]);
    }
}
