//! Synchronous store owner used by the host's single compactor. Index publication
//! follows sync; recovery inspection exposes no mutation or normal read path.
mod insert;
mod read;
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
use std::{
    fs::{self, File},
    io,
    sync::{Arc, Mutex, MutexGuard},
};

pub use read::{Payload, Read, Reader};
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
    shared: Arc<Shared>,
    config: Config,
    io_memory: Arc<Budget>,
}

struct Shared {
    directory: Directory,
    tickets: Arc<Tickets>,
    metadata: Arc<Budget>,
    state: Mutex<State>,
}

struct State {
    index: Index,
    segments: Vec<Segment, BudgetAllocator>,
    failed: bool,
}

impl State {
    fn healthy(&self) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::other(
                "chunk store failed; explicit recovery required",
            ));
        }
        Ok(())
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
            segments: state.segments.len(),
            encoded_bytes: state.segments.iter().map(|s| s.end).sum(),
            index_bytes: state.index.allocated_bytes(),
            batch_table_bytes: state
                .segments
                .iter()
                .map(|s| s.batches.capacity() * size_of::<BatchLocation>())
                .sum(),
            failed: state.failed,
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
            shared: Arc::new(Shared {
                directory,
                tickets,
                state: Mutex::new(State {
                    index: Index::new(Arc::clone(&metadata)),
                    segments: Vec::new_in(BudgetAllocator::new(Arc::clone(&metadata))),
                    failed: false,
                }),
                metadata,
            }),
            config,
            io_memory,
        }
    }

    pub fn config(&self) -> Config {
        self.config
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
