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
mod format;

use format::{Record, RecordKind, decode, decode_header, encode, encode_header};

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

impl StagingLog {
    fn empty(file: File, image_bytes: u64) -> Self {
        Self {
            file,
            image_bytes,
            next_offset: BLOCK_SIZE as u64,
            appended: 0,
            durable: 0,
            blocks: BTreeMap::new(),
            poisoned: false,
            recovered_tail_bytes: 0,
        }
    }

    /// Creates a zero-initialized logical image. Never replaces an existing log.
    /// This does not import a raw image or implement a compacted base manifest.
    pub fn create(path: impl AsRef<Path>, image_bytes: u64) -> Result<Self> {
        let path = path.as_ref();
        if image_bytes == 0 || !image_bytes.is_multiple_of(BLOCK_SIZE as u64) {
            return Err(Error::Range);
        }
        let file = direct::open(path, true)?;
        let header = encode_header(image_bytes);
        direct::write(&file, &header, 0)?;
        file.sync_all()?;
        // A synced file does not by itself make a newly created name durable.
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        File::open(parent)?.sync_all()?;
        Ok(Self::empty(file, image_bytes))
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
        let image_bytes = decode_header(header.bytes()).ok_or(Error::Header)?;
        let slots = (original_length - BLOCK_SIZE as u64) / RECORD_SIZE as u64;
        let mut committed_slots = 0;
        let mut buffer = Aligned::new(RECORD_SIZE);
        // Fixed slots prevent guest payloads resembling a fence from becoming
        // metadata. Reverse scan establishes the bound before validating replay.
        for slot in (0..slots).rev() {
            let position = BLOCK_SIZE as u64 + slot * RECORD_SIZE as u64;
            direct::read(&file, &mut buffer, position)?;
            if decode(buffer.bytes()).is_some_and(|record| record.kind == RecordKind::Fence) {
                committed_slots = slot + 1;
                break;
            }
        }
        let mut log = Self::empty(file, image_bytes);
        log.next_offset += committed_slots * RECORD_SIZE as u64;
        for slot in 0..committed_slots {
            let position = BLOCK_SIZE as u64 + slot * RECORD_SIZE as u64;
            direct::read(&log.file, &mut buffer, position)?;
            let record = decode(buffer.bytes()).ok_or(Error::Corrupt(position))?;
            if record.kind == RecordKind::Fence {
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
        match record.kind {
            RecordKind::Write => {
                self.blocks.insert(record.offset, position);
            }
            RecordKind::Zero => {
                let end = record.offset + record.length;
                self.blocks
                    .retain(|offset, _| *offset < record.offset || *offset >= end);
            }
            RecordKind::Fence => {}
        }
    }

    fn append(
        &mut self,
        kind: RecordKind,
        offset: u64,
        length: u64,
        payload: &[u8],
    ) -> Result<u64> {
        let sequence = if kind == RecordKind::Fence {
            self.appended
        } else {
            self.appended.checked_add(1).ok_or(Error::Exhausted)?
        };
        let record = Record {
            kind,
            sequence,
            offset,
            length,
        };
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
        self.appended = sequence;
        Ok(sequence)
    }

    /// Returns the final append sequence, or None for an empty request.
    /// A multi-block request is ordered but has no multi-block atomicity promise.
    pub fn write(&mut self, offset: u64, bytes: &[u8]) -> Result<Option<u64>> {
        self.healthy()?;
        self.validate_range(offset, bytes.len() as u64)?;
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(Error::RequestTooLarge);
        }
        let (blocks, _) = bytes.as_chunks::<BLOCK_SIZE>();
        for (index, block) in blocks.iter().enumerate() {
            self.append(
                RecordKind::Write,
                offset + (index * BLOCK_SIZE) as u64,
                BLOCK_SIZE as u64,
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
        self.append(RecordKind::Zero, offset, length, &[]).map(Some)
    }

    /// The fence and preceding writes are covered by one fdatasync. E advances
    /// only after it succeeds. Dropping the log does not perform a FLUSH.
    pub fn flush(&mut self) -> Result<u64> {
        self.healthy()?;
        if self.appended != self.durable {
            self.append(RecordKind::Fence, 0, 0, &[])?;
            if let Err(error) = direct::sync_data(&self.file) {
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
        let (blocks, _) = result.as_chunks_mut::<BLOCK_SIZE>();
        for (index, block) in blocks.iter_mut().enumerate() {
            let logical_offset = offset + (index * BLOCK_SIZE) as u64;
            if let Some(&position) = self.blocks.get(&logical_offset) {
                direct::read(&self.file, &mut buffer, position)?;
                let record = decode(buffer.bytes()).ok_or(Error::Corrupt(position))?;
                if record.kind != RecordKind::Write || record.offset != logical_offset {
                    return Err(Error::Corrupt(position));
                }
                block.copy_from_slice(&buffer.bytes()[BLOCK_SIZE..]);
            }
        }
        Ok(result)
    }
}

impl Drop for StagingLog {
    fn drop(&mut self) {
        // A duplicated or temporarily inherited descriptor can outlive this
        // handle. Explicit unlock releases our flock without waiting for the
        // last close of that open file description. Drop does not FLUSH data.
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_writer_releases_lock_even_with_a_duplicate_descriptor() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = directory.path().join("image.log");
        let mut log = StagingLog::create(&path, (4 * BLOCK_SIZE) as u64).unwrap();
        log.write(0, &vec![1; BLOCK_SIZE]).unwrap();
        log.flush().unwrap();
        // dup and fork share the open file description and its flock. Keep a
        // duplicate alive to reproduce delayed last-close without a timing race.
        let duplicate = log.file.try_clone().unwrap();
        assert!(StagingLog::open(&path).is_err());
        drop(log);
        let reopened = StagingLog::open(&path).unwrap();
        assert_eq!(reopened.read(0, BLOCK_SIZE).unwrap(), vec![1; BLOCK_SIZE]);
        drop(duplicate);
        // Closing the old duplicate must not release the new writer's lock.
        assert!(StagingLog::open(&path).is_err());
    }

    #[test]
    fn short_write_poisoning_preserves_the_previous_flush() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = directory.path().join("image.log");
        let mut log = StagingLog::create(&path, (4 * BLOCK_SIZE) as u64).unwrap();
        log.write(0, &[1; BLOCK_SIZE]).unwrap();
        log.flush().unwrap();
        let before = log.status();

        direct::faults::inject(direct::faults::Fault::ShortWrite);
        assert!(
            matches!(log.write(0, &[2; BLOCK_SIZE]), Err(Error::Io(error))
            if error.kind() == io::ErrorKind::WriteZero)
        );
        assert_eq!(log.status(), before);
        assert_eq!(
            log.file.metadata().unwrap().len(),
            before.log_bytes + BLOCK_SIZE as u64
        );
        assert!(matches!(log.flush(), Err(Error::Poisoned)));
        assert!(matches!(
            log.write(0, &[3; BLOCK_SIZE]),
            Err(Error::Poisoned)
        ));
        drop(log);

        let recovered = StagingLog::open(path).unwrap();
        assert_eq!(recovered.status().durable, before.durable);
        assert_eq!(recovered.status().recovered_tail_bytes, BLOCK_SIZE as u64);
        assert_eq!(recovered.read(0, BLOCK_SIZE).unwrap(), [1; BLOCK_SIZE]);
    }

    #[test]
    fn failed_sync_never_acknowledges_the_new_prefix() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = directory.path().join("image.log");
        let mut log = StagingLog::create(&path, (4 * BLOCK_SIZE) as u64).unwrap();
        log.write(0, &[1; BLOCK_SIZE]).unwrap();
        let durable = log.flush().unwrap();
        log.write(0, &[2; BLOCK_SIZE]).unwrap();

        direct::faults::inject(direct::faults::Fault::Sync);
        assert!(matches!(log.flush(), Err(Error::Io(error))
            if error.raw_os_error() == Some(libc::EIO)));
        assert_eq!(log.status().durable, durable);
        assert!(matches!(log.flush(), Err(Error::Poisoned)));
        assert!(matches!(
            log.zero(0, BLOCK_SIZE as u64),
            Err(Error::Poisoned)
        ));
        assert!(matches!(log.read(0, BLOCK_SIZE), Err(Error::Poisoned)));
        drop(log);

        // A complete but unacknowledged fence may survive; recovery must still
        // produce a complete batch, not assume the failed sync erased it.
        let recovered = StagingLog::open(path).unwrap();
        assert_eq!(recovered.status().durable, durable + 1);
        assert_eq!(recovered.read(0, BLOCK_SIZE).unwrap(), [2; BLOCK_SIZE]);
    }

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
