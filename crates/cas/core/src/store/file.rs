//! Synchronous store owner used by the host's single compactor. Index publication
//! follows sync; recovery inspection exposes no mutation or normal read path.
mod recovery;

use super::format::{Builder, Header, MAX_BATCH_BYTES, MAX_CHUNKS, SegmentHeader};
use crate::{
    BLOCK_SIZE,
    aligned::AlignedBuffer,
    budget::{Budget, BudgetAllocator},
    chunk::Chunk,
    chunk_index::{Address, Hash, Index, MAX_SEGMENT_BYTES},
    direct,
    directory::Directory,
    encoding::require,
    segments::{self, Tickets},
};
use allocator_api2::vec::Vec;
use arrayvec::ArrayVec;
use std::{
    fs::{self, File},
    io,
    sync::Arc,
};

pub use recovery::Inspection;

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub store: [u8; 16],
    pub segment_bytes: u64,
}

impl Config {
    fn validate(self) -> io::Result<()> {
        require(
            self.store != [0; 16]
                && ((MAX_BATCH_BYTES + BLOCK_SIZE) as u64..=MAX_SEGMENT_BYTES)
                    .contains(&self.segment_bytes)
                && self.segment_bytes.is_multiple_of(BLOCK_SIZE as u64),
            "invalid chunk store geometry/identity",
        )
    }
}

#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct Inserted {
    pub written: usize,
    pub reused: usize,
    pub encoded_bytes: usize,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Status {
    pub chunks: usize,
    pub segments: usize,
    pub encoded_bytes: u64,
    pub index_bytes: usize,
    pub batch_table_bytes: usize,
    pub failed: bool,
}

pub struct Store {
    directory: Directory,
    tickets: Arc<Tickets>,
    config: Config,
    metadata: Arc<Budget>,
    io_memory: Arc<Budget>,
    index: Index,
    segments: Vec<Segment, BudgetAllocator>,
    failed: bool,
}

impl Store {
    /// The governor reserves namespace/output disk capacity before calling this
    /// or insert/recovery. This layer preallocates but does not measure host space.
    pub fn create(
        tickets: Arc<Tickets>,
        config: Config,
        metadata: Arc<Budget>,
        io_memory: Arc<Budget>,
    ) -> io::Result<Self> {
        config.validate()?;
        let path = tickets.root().join("chunks");
        fs::create_dir(&path)?;
        File::open(tickets.root())?.sync_all()?;
        let directory = Directory::open(&path)?;
        Ok(Self::empty(directory, tickets, config, metadata, io_memory))
    }

    fn empty(
        directory: Directory,
        tickets: Arc<Tickets>,
        config: Config,
        metadata: Arc<Budget>,
        io_memory: Arc<Budget>,
    ) -> Self {
        Self {
            directory,
            tickets,
            config,
            io_memory,
            index: Index::new(Arc::clone(&metadata)),
            segments: Vec::new_in(BudgetAllocator::new(Arc::clone(&metadata))),
            metadata,
            failed: false,
        }
    }

    pub fn config(&self) -> Config {
        self.config
    }

    pub fn status(&self) -> Status {
        Status {
            chunks: self.index.len(),
            segments: self.segments.len(),
            encoded_bytes: self.segments.iter().map(|s| s.end).sum(),
            index_bytes: self.index.allocated_bytes(),
            batch_table_bytes: self
                .segments
                .iter()
                .map(|s| s.batches.capacity() * size_of::<BatchLocation>())
                .sum(),
            failed: self.failed,
        }
    }

    fn healthy(&self) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::other(
                "chunk store failed; explicit recovery required",
            ));
        }
        Ok(())
    }

    /// Pure lookup. The returned plan pins the actual IO file through completion.
    pub fn plan(&self, hash: Hash) -> io::Result<Option<Read>> {
        self.healthy()?;
        let Some(address) = self.index.get(&hash) else {
            return Ok(None);
        };
        let segment = self
            .segments
            .binary_search_by_key(&address.segment(), |s| s.header.number)
            .ok()
            .map(|index| &self.segments[index])
            .ok_or_else(|| io::Error::other("chunk index references a missing segment"))?;
        let batch = segment
            .batches
            .partition_point(|batch| u64::from(batch.offset) < address.offset())
            .checked_sub(1)
            .map(|index| segment.batches[index])
            .ok_or_else(|| io::Error::other("chunk address has no batch header"))?;
        require(
            address.offset() < batch.end(),
            "chunk address outside batch",
        )?;
        Ok(Some(Read {
            file: Arc::clone(&segment.file),
            address,
            batch,
            hash,
            metadata: Arc::clone(&self.metadata),
        }))
    }

    pub fn insert(&mut self, chunks: &[Chunk<'_>]) -> io::Result<Inserted> {
        self.healthy()?;
        require(
            chunks.len() <= MAX_CHUNKS,
            "chunk insertion exceeds batch bound",
        )?;
        let mut missing = ArrayVec::<Chunk<'_>, MAX_CHUNKS>::new();
        for &chunk in chunks {
            if self.index.get(&chunk.hash()).is_none()
                && !missing.iter().any(|old| old.hash() == chunk.hash())
            {
                missing.push(chunk);
            }
        }
        let mut inserted = Inserted {
            reused: chunks.len() - missing.len(),
            ..Inserted::default()
        };
        if missing.is_empty() {
            return Ok(inserted);
        }
        self.index.reserve(missing.len())?;
        let mut builder = Builder::try_new_in(
            missing.len(),
            BudgetAllocator::new(Arc::clone(&self.io_memory)),
        )?;
        for &chunk in &missing {
            builder.push(chunk)?;
        }
        let bytes = ((missing.len() + 1) * BLOCK_SIZE) as u64;
        if self
            .segments
            .last()
            .is_none_or(|s| bytes > s.header.capacity - s.end)
        {
            reserve(&mut self.segments, 1)?;
            // Create allocates its batch table and header buffer before file IO.
            // A failed header creation poisons both the ticket and store owners.
            match Segment::create(
                &self.directory,
                &self.tickets,
                self.config,
                Arc::clone(&self.metadata),
            ) {
                Ok(segment) => self.segments.push(segment),
                Err(error) => {
                    self.failed |= self.tickets.status().failed;
                    return Err(error);
                }
            }
        }
        let segment = self.segments.last_mut().expect("insertion owns a segment");
        reserve(&mut segment.batches, 1)?;
        let next_batch = segment
            .next_batch
            .checked_add(1)
            .ok_or_else(|| io::Error::other("chunk batch IDs exhausted"))?;
        let next_ordinal = segment
            .next_ordinal
            .checked_add(missing.len() as u64)
            .ok_or_else(|| io::Error::other("chunk ordinals exhausted"))?;
        let batch = builder.seal(
            segment.header.number,
            segment.next_batch,
            segment.next_ordinal,
        )?;
        self.failed = true;
        // Recovery may have truncated preallocated tail extents. Reserve this
        // output range even when the original segment creation reserved more.
        direct::preallocate(&segment.file, segment.end, bytes)?;
        direct::write_bytes(&segment.file, batch.bytes(), segment.end)?;
        direct::sync_data(&segment.file)?;
        for (index, chunk) in missing.iter().enumerate() {
            self.index.insert(
                chunk.hash(),
                Address::new(
                    segment.header.number,
                    segment.end + ((index + 1) * BLOCK_SIZE) as u64,
                )?,
            )?;
        }
        segment.batches.push(BatchLocation {
            offset: segment.end as u32,
            chunks: missing.len() as u16,
        });
        segment.end += bytes;
        segment.file_bytes = segment.end;
        segment.next_batch = next_batch;
        segment.next_ordinal = next_ordinal;
        self.failed = false;
        inserted.written = missing.len();
        inserted.encoded_bytes = bytes as usize;
        Ok(inserted)
    }
}

#[derive(Clone, Copy)]
struct BatchLocation {
    offset: u32,
    chunks: u16,
}

impl BatchLocation {
    fn end(self) -> u64 {
        u64::from(self.offset) + (u64::from(self.chunks) + 1) * BLOCK_SIZE as u64
    }
}

struct Segment {
    file: Arc<File>,
    header: SegmentHeader,
    batches: Vec<BatchLocation, BudgetAllocator>,
    end: u64,
    file_bytes: u64,
    next_batch: u64,
    next_ordinal: u64,
}

impl Segment {
    fn create(
        directory: &Directory,
        tickets: &Tickets,
        config: Config,
        metadata: Arc<Budget>,
    ) -> io::Result<Self> {
        let mut batches = Vec::new_in(BudgetAllocator::new(Arc::clone(&metadata)));
        reserve(&mut batches, 1)?;
        let mut scratch = AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(metadata))?;
        tickets.allocate(|number| {
            let header = SegmentHeader {
                store: config.store,
                number,
                capacity: config.segment_bytes,
            };
            header.encode_into(scratch.as_mut_slice())?;
            let file = direct::open(&directory.path.join(segments::name(number)), true)?;
            direct::Alignment::query(&file)?;
            direct::preallocate(&file, 0, config.segment_bytes)?;
            direct::write_bytes(&file, scratch.as_slice(), 0)?;
            file.sync_all()?;
            directory.sync()?;
            Ok(Self {
                file: Arc::new(file),
                header,
                batches,
                end: BLOCK_SIZE as u64,
                file_bytes: BLOCK_SIZE as u64,
                next_batch: 1,
                next_ordinal: 1,
            })
        })
    }
}

/// A stable header location is captured from metadata, never inferred by
/// scanning preceding payload pages for magic.
pub struct Read {
    file: Arc<File>,
    address: Address,
    batch: BatchLocation,
    hash: Hash,
    metadata: Arc<Budget>,
}

impl Read {
    pub fn address(&self) -> Address {
        self.address
    }

    pub fn load(&self, destination: &mut [u8]) -> io::Result<()> {
        require(destination.len() == BLOCK_SIZE, "chunk destination size")?;
        let mut scratch = AlignedBuffer::try_new_in(
            BLOCK_SIZE,
            BudgetAllocator::new(Arc::clone(&self.metadata)),
        )?;
        direct::read_bytes(
            &self.file,
            scratch.as_mut_slice(),
            u64::from(self.batch.offset),
        )?;
        let header = Header::decode(scratch.as_slice())?;
        require(
            header.segment() == self.address.segment()
                && header.descriptors().len() == usize::from(self.batch.chunks),
            "chunk read batch identity",
        )?;
        let index = ((self.address.offset() - u64::from(self.batch.offset)) / BLOCK_SIZE as u64 - 1)
            as usize;
        let descriptor = header
            .descriptors()
            .nth(index)
            .ok_or_else(|| io::Error::other("missing chunk descriptor"))?;
        require(descriptor.hash == self.hash, "chunk read hash mismatch")?;
        direct::read_bytes(&self.file, destination, self.address.offset())?;
        require(
            crc32fast::hash(destination) == descriptor.crc,
            "chunk read CRC",
        )
    }
}

fn reserve<T>(values: &mut Vec<T, BudgetAllocator>, additional: usize) -> io::Result<()> {
    values.try_reserve(additional).map_err(|_| {
        io::Error::new(
            io::ErrorKind::OutOfMemory,
            "chunk metadata budget exhausted",
        )
    })
}

#[cfg(test)]
mod tests;
