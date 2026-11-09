use super::*;

/// Cloneable lookup over the same index and file owners as the single writer.
#[derive(Clone)]
pub struct Reader {
    pub(super) shared: Arc<Shared>,
}

impl Reader {
    pub(crate) fn record_compaction_reads(&self, bytes: u64) {
        self.shared
            .compaction_reads
            .fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
    }
    /// Bounded by the caller's metadata budget; a shipping pass owns a stable
    /// inventory while insertions continue. This snapshot does not pin payloads.
    pub fn inventory(&self, metadata: Arc<Budget>) -> io::Result<Vec<Hash, BudgetAllocator>> {
        let state = self.shared.lock();
        state.healthy()?;
        let mut hashes = Vec::new_in(BudgetAllocator::new(metadata));
        hashes
            .try_reserve_exact(state.index.len())
            .map_err(|_| io::ErrorKind::OutOfMemory)?;
        hashes.extend(state.index.entries().map(|(hash, _, _)| *hash));
        Ok(hashes)
    }
    pub fn plan(&self, hash: Hash) -> io::Result<Option<Read>> {
        self.shared.plan(hash)
    }

    pub fn chunk_size(&self) -> ChunkSize {
        self.shared.chunk_size
    }

    pub fn status(&self) -> Status {
        self.shared.status()
    }
}

impl Shared {
    pub(super) fn plan(&self, hash: Hash) -> io::Result<Option<Read>> {
        let state = self.lock();
        state.healthy()?;
        if state.collecting {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "chunk collection in progress",
            ));
        }
        let Some(address) = state.index.get(&hash) else {
            return Ok(None);
        };
        let segment = state
            .segments
            .binary_search_by_key(&address.segment(), |s| s.header.number)
            .ok()
            .map(|index| &state.segments[index])
            .ok_or_else(|| io::Error::other("chunk index references a missing segment"))?;
        let batch_index = segment
            .batches
            .partition_point(|batch| u64::from(batch.offset) < address.offset())
            .checked_sub(1)
            .ok_or_else(|| io::Error::other("chunk address has no batch header"))?;
        let batch = segment.batches[batch_index];
        require(
            address.offset() < batch.end(),
            "chunk address outside batch",
        )?;
        Ok(Some(Read {
            file: Arc::clone(&segment.file),
            address,
            batch,
            batch_id: batch_index as u64 + 1,
            hash,
            metadata: Arc::clone(&self.metadata),
        }))
    }
}

/// A stable header location captured from metadata, never inferred by scanning
/// preceding payload pages for magic. The actual IO file remains pinned.
pub struct Read {
    file: Arc<File>,
    address: Address,
    batch: BatchLocation,
    batch_id: u64,
    hash: Hash,
    metadata: Arc<Budget>,
}

impl Read {
    pub fn address(&self) -> Address {
        self.address
    }
    pub fn file(&self) -> &File {
        &self.file
    }
    pub fn header_offset(&self) -> u64 {
        u64::from(self.batch.offset)
    }

    /// Validate a completed header read before permitting payload verification.
    pub fn payload(&self, page: &[u8]) -> io::Result<Payload> {
        let header = Header::decode(page)?;
        require(
            header.chunk_size() == self.batch.chunk_size,
            "chunk read geometry",
        )?;
        require(
            batch_identity(
                &header,
                self.address.segment(),
                self.batch_id,
                self.header_offset(),
                self.batch.chunks,
            ),
            "chunk read batch identity",
        )?;
        let index = ((self.address.offset() - self.header_offset() - BLOCK_SIZE as u64)
            / self.batch.chunk_size.bytes() as u64) as usize;
        let descriptor = header
            .descriptors()
            .nth(index)
            .ok_or_else(|| io::Error::other("missing chunk descriptor"))?;
        require(descriptor.hash == self.hash, "chunk read hash mismatch")?;
        Ok(Payload {
            file: Arc::clone(&self.file),
            offset: self.address.offset(),
            crc: descriptor.crc,
            bytes: self.batch.chunk_size.bytes(),
        })
    }

    pub fn load(&self, destination: &mut [u8]) -> io::Result<()> {
        require(
            destination.len() == self.batch.chunk_size.bytes(),
            "chunk destination size",
        )?;
        let mut scratch = AlignedBuffer::try_new_in(
            BLOCK_SIZE,
            BudgetAllocator::new(Arc::clone(&self.metadata)),
        )?;
        self.load_with_scratch(scratch.as_mut_slice(), destination)
    }

    /// Reuse aligned caller-owned header scratch on a bounded service worker.
    pub fn load_with_scratch(&self, scratch: &mut [u8], destination: &mut [u8]) -> io::Result<()> {
        require(
            scratch.len() == BLOCK_SIZE && destination.len() == self.batch.chunk_size.bytes(),
            "chunk scratch/destination size",
        )?;
        direct::read_bytes(&self.file, scratch, self.header_offset())?;
        self.payload(scratch)?.load(destination)
    }
}

/// A verified immutable descriptor and actual file pin for one payload CQE.
pub struct Payload {
    file: Arc<File>,
    offset: u64,
    crc: u32,
    bytes: usize,
}

impl Payload {
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn file(&self) -> &File {
        &self.file
    }
    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn verify(&self, bytes: &[u8]) -> io::Result<()> {
        require(
            bytes.len() == self.bytes && crc32fast::hash(bytes) == self.crc,
            "chunk read CRC",
        )
    }

    pub fn load(&self, destination: &mut [u8]) -> io::Result<()> {
        require(destination.len() == self.bytes, "chunk destination size")?;
        direct::read_bytes(&self.file, destination, self.offset)?;
        self.verify(destination)
    }
}
