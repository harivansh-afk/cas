use super::*;

/// Cloneable lookup over the same index and file owners as the single writer.
#[derive(Clone)]
pub struct Reader {
    pub(super) shared: Arc<Shared>,
}

impl Reader {
    pub fn plan(&self, hash: Hash) -> io::Result<Option<Read>> {
        self.shared.plan(hash)
    }

    pub fn status(&self) -> Status {
        self.shared.status()
    }
}

impl Shared {
    pub(super) fn plan(&self, hash: Hash) -> io::Result<Option<Read>> {
        let state = self.lock();
        state.healthy()?;
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
            header.segment() == self.address.segment()
                && header.batch() == self.batch_id
                && header.first() == self.header_offset() / BLOCK_SIZE as u64 - self.batch_id + 1
                && header.descriptors().len() == usize::from(self.batch.chunks),
            "chunk read batch identity",
        )?;
        let index =
            ((self.address.offset() - self.header_offset()) / BLOCK_SIZE as u64 - 1) as usize;
        let descriptor = header
            .descriptors()
            .nth(index)
            .ok_or_else(|| io::Error::other("missing chunk descriptor"))?;
        require(descriptor.hash == self.hash, "chunk read hash mismatch")?;
        Ok(Payload {
            file: Arc::clone(&self.file),
            offset: self.address.offset(),
            crc: descriptor.crc,
        })
    }

    pub fn load(&self, destination: &mut [u8]) -> io::Result<()> {
        require(destination.len() == BLOCK_SIZE, "chunk destination size")?;
        let mut scratch = AlignedBuffer::try_new_in(
            BLOCK_SIZE,
            BudgetAllocator::new(Arc::clone(&self.metadata)),
        )?;
        direct::read_bytes(&self.file, scratch.as_mut_slice(), self.header_offset())?;
        self.payload(scratch.as_slice())?.load(destination)
    }
}

/// A verified immutable descriptor and actual file pin for one payload CQE.
pub struct Payload {
    file: Arc<File>,
    offset: u64,
    crc: u32,
}

impl Payload {
    pub fn file(&self) -> &File {
        &self.file
    }
    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn verify(&self, bytes: &[u8]) -> io::Result<()> {
        require(
            bytes.len() == BLOCK_SIZE && crc32fast::hash(bytes) == self.crc,
            "chunk read CRC",
        )
    }

    pub fn load(&self, destination: &mut [u8]) -> io::Result<()> {
        require(destination.len() == BLOCK_SIZE, "chunk destination size")?;
        direct::read_bytes(&self.file, destination, self.offset)?;
        self.verify(destination)
    }
}
