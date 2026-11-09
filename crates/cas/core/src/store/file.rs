//! Synchronous store owner used by the host's single compactor. Index publication
//! follows sync; recovery inspection exposes no mutation or normal read path.
mod collection;
mod geometry;
mod insert;
mod read;
mod recovery;

use super::format::{Builder, Header, MAX_BATCH_BYTES, MAX_CHUNKS, SegmentHeader};
use crate::{
    BLOCK_SIZE,
    aligned::AlignedBuffer,
    budget::{Budget, BudgetAllocator},
    chunk::{Chunk, ChunkSize},
    chunk_index::{Address, Hash, Index, MAX_SEGMENT_BYTES},
    direct,
    directory::Directory,
    encoding::require,
    segments::{self, Tickets},
};
use allocator_api2::vec::Vec;
use std::{
    fs::{self, File},
    io,
    sync::{Arc, Mutex, MutexGuard},
};

pub use collection::{Collected, Collection, Sweep, Victim};
pub use read::{Payload, Read, Reader};
pub use recovery::Inspection;

/// Chunk segments live under this directory of the store root.
pub(crate) const DIRECTORY: &str = "chunks";

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub store: [u8; 16],
    pub segment_bytes: u64,
}

impl Config {
    pub fn validate(self) -> io::Result<()> {
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
    pub chunk_bytes: usize,
    pub segments: usize,
    pub encoded_bytes: u64,
    pub index_bytes: usize,
    pub index_memory: crate::chunk_index::Memory,
    pub batch_table_bytes: usize,
    pub failed: bool,
    pub collecting: bool,
    pub store_bytes_appended: u64,
    pub collection_rewrite_bytes: u64,
    pub compaction_read_bytes: u64,
}

pub struct Store {
    shared: Arc<Shared>,
    config: Config,
    io_memory: Arc<Budget>,
}

struct Shared {
    chunk_size: ChunkSize,
    directory: Directory,
    tickets: Arc<Tickets>,
    metadata: Arc<Budget>,
    state: Mutex<State>,
    compaction_reads: std::sync::atomic::AtomicU64,
}

struct State {
    index: Index,
    segments: Vec<Segment, BudgetAllocator>,
    failed: bool,
    collecting: bool,
    store_bytes_appended: u64,
    collection_rewrite_bytes: u64,
}

impl State {
    fn healthy(&self) -> io::Result<()> {
        crate::encoding::require_healthy(
            self.failed,
            "chunk store failed; explicit recovery required",
        )
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| {
            let mut state = poisoned.into_inner();
            state.failed = true;
            state
        })
    }

    fn status(&self) -> Status {
        let state = self.lock();
        Status {
            chunks: state.index.len(),
            chunk_bytes: self.chunk_size.bytes(),
            segments: state.segments.len(),
            encoded_bytes: state.segments.iter().map(|s| s.end).sum(),
            index_bytes: state.index.allocated_bytes(),
            index_memory: state.index.memory(),
            batch_table_bytes: state
                .segments
                .iter()
                .map(|s| s.batches.capacity() * size_of::<BatchLocation>())
                .sum(),
            failed: state.failed,
            collecting: state.collecting,
            store_bytes_appended: state.store_bytes_appended,
            collection_rewrite_bytes: state.collection_rewrite_bytes,
            compaction_read_bytes: self
                .compaction_reads
                .load(std::sync::atomic::Ordering::Relaxed),
        }
    }
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
        Self::create_sized(tickets, config, ChunkSize::K4, metadata, io_memory)
    }

    pub fn create_sized(
        tickets: Arc<Tickets>,
        config: Config,
        chunk_size: ChunkSize,
        metadata: Arc<Budget>,
        io_memory: Arc<Budget>,
    ) -> io::Result<Self> {
        config.validate()?;
        require(
            config.segment_bytes >= (2 * BLOCK_SIZE + MAX_CHUNKS * chunk_size.bytes()) as u64,
            "chunk segment too small for geometry",
        )?;
        let path = tickets.root().join(DIRECTORY);
        fs::create_dir(&path)?;
        direct::sync_all(tickets.root_file())?;
        let directory = Directory::open(&path)?;
        geometry::create(&tickets, config, chunk_size, Arc::clone(&metadata))?;
        Ok(Self::empty(
            directory, tickets, config, chunk_size, metadata, io_memory,
        ))
    }

    fn empty(
        directory: Directory,
        tickets: Arc<Tickets>,
        config: Config,
        chunk_size: ChunkSize,
        metadata: Arc<Budget>,
        io_memory: Arc<Budget>,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                chunk_size,
                compaction_reads: std::sync::atomic::AtomicU64::new(0),
                directory,
                tickets,
                state: Mutex::new(State {
                    index: Index::new(Arc::clone(&metadata)),
                    segments: Vec::new_in(BudgetAllocator::new(Arc::clone(&metadata))),
                    failed: false,
                    collecting: false,
                    store_bytes_appended: 0,
                    collection_rewrite_bytes: 0,
                }),
                metadata,
            }),
            config,
            io_memory,
        }
    }

    pub fn chunk_size(&self) -> ChunkSize {
        self.shared.chunk_size
    }

    pub fn recorded_geometry(
        tickets: &Tickets,
        metadata: Arc<Budget>,
    ) -> io::Result<(Config, ChunkSize)> {
        geometry::config(tickets, metadata)
    }

    pub fn config(&self) -> Config {
        self.config
    }

    pub fn tickets(&self) -> &Arc<Tickets> {
        &self.shared.tickets
    }

    pub fn status(&self) -> Status {
        self.shared.status()
    }

    pub fn reader(&self) -> io::Result<Reader> {
        self.shared.lock().healthy()?;
        Ok(Reader {
            shared: Arc::clone(&self.shared),
        })
    }

    pub fn plan(&self, hash: Hash) -> io::Result<Option<Read>> {
        self.shared.plan(hash)
    }
}

/// A segment is block 0 (its header) followed by contiguous batches, each one
/// header block and then fixed-size chunks, with chunk ordinals counted from 1
/// across the segment. Subtract the preceding segment/batch headers from the
/// block offset, then divide by the number of guest blocks per chunk.
fn batch_identity(header: &Header, segment: u64, batch: u64, offset: u64, chunks: u16) -> bool {
    header.segment() == segment
        && header.batch() == batch
        && header.first() == (offset / BLOCK_SIZE as u64 - batch) / header.chunk_size().blocks() + 1
        && header.descriptors().len() == usize::from(chunks)
}

#[derive(Clone, Copy)]
struct BatchLocation {
    offset: u32,
    chunks: u16,
    chunk_size: ChunkSize,
}

impl BatchLocation {
    fn end(self) -> u64 {
        u64::from(self.offset)
            + BLOCK_SIZE as u64
            + u64::from(self.chunks) * self.chunk_size.bytes() as u64
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
    sealed: bool,
}

impl Segment {
    fn create(
        directory: &Directory,
        tickets: &Tickets,
        config: Config,
        chunk_size: ChunkSize,
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
                chunk_size,
            };
            header.encode_into(scratch.as_mut_slice())?;
            let file = direct::open(&directory.path().join(segments::name(number)), true)?;
            direct::Alignment::query(&file)?;
            direct::preallocate(&file, 0, config.segment_bytes)?;
            direct::write_bytes(&file, scratch.as_slice(), 0)?;
            direct::sync_all(&file)?;
            directory.sync()?;
            Ok(Self {
                file: Arc::new(file),
                header,
                batches,
                end: BLOCK_SIZE as u64,
                file_bytes: BLOCK_SIZE as u64,
                next_batch: 1,
                next_ordinal: 1,
                sealed: false,
            })
        })
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
