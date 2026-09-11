//! Fixed chunk framing, as specified before implementation in storage-format.md.
use crate::{
    BLOCK_SIZE,
    aligned::AlignedBuffer,
    chunk::Chunk,
    chunk_index::{Hash, MAX_SEGMENT, MAX_SEGMENT_BYTES},
    encoding::{checksum, put16, put32, put64, require, u16_at, u32_at, u64_at},
};
use std::io;

pub const MAX_CHUNKS: usize = 63;
pub const MAX_BATCH_BYTES: usize = (MAX_CHUNKS + 1) * BLOCK_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentHeader {
    pub store: [u8; 16],
    pub number: u64,
    pub capacity: u64,
}

impl SegmentHeader {
    fn validate(self) -> io::Result<()> {
        require(self.store != [0; 16], "empty chunk store identity")?;
        require(
            (1..=MAX_SEGMENT).contains(&self.number),
            "chunk segment number",
        )?;
        require(
            self.capacity >= (3 * BLOCK_SIZE) as u64
                && self.capacity <= MAX_SEGMENT_BYTES
                && self.capacity.is_multiple_of(BLOCK_SIZE as u64),
            "chunk segment capacity",
        )
    }

    pub fn encode(self) -> io::Result<AlignedBuffer> {
        self.validate()?;
        let mut buffer = AlignedBuffer::new(BLOCK_SIZE);
        let bytes = buffer.as_mut_slice();
        bytes[..8].copy_from_slice(b"CASCHS02");
        put32(bytes, 8, 2);
        put32(bytes, 12, BLOCK_SIZE as u32);
        bytes[16..32].copy_from_slice(&self.store);
        put64(bytes, 56, self.number);
        put64(bytes, 64, self.capacity);
        put32(bytes, 4092, checksum(bytes, 4092));
        Ok(buffer)
    }

    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        require(bytes.len() == BLOCK_SIZE, "chunk segment header size")?;
        require(
            &bytes[..8] == b"CASCHS02"
                && u32_at(bytes, 8) == 2
                && u32_at(bytes, 12) == BLOCK_SIZE as u32,
            "chunk segment version",
        )?;
        require(
            u32_at(bytes, 4092) == checksum(bytes, 4092),
            "chunk segment CRC",
        )?;
        require(
            bytes[32..56]
                .iter()
                .chain(&bytes[72..4092])
                .all(|&b| b == 0),
            "chunk segment reserved bytes",
        )?;
        let header = Self {
            store: bytes[16..32].try_into().unwrap(),
            number: u64_at(bytes, 56),
            capacity: u64_at(bytes, 64),
        };
        header.validate()?;
        Ok(header)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor {
    pub hash: Hash,
    pub payload_offset: u32,
    pub crc: u32,
}

impl Descriptor {
    fn decode(bytes: &[u8]) -> Self {
        Self {
            hash: bytes[..32].try_into().unwrap(),
            payload_offset: u32_at(bytes, 32),
            crc: u32_at(bytes, 40),
        }
    }
}

pub struct Header<'a> {
    bytes: &'a [u8],
    count: usize,
}

impl<'a> Header<'a> {
    pub fn decode(bytes: &'a [u8]) -> io::Result<Self> {
        require(bytes.len() == BLOCK_SIZE, "chunk batch header size")?;
        require(
            &bytes[..8] == b"CASCHB02" && u16_at(bytes, 8) == 2 && u16_at(bytes, 10) == 1,
            "chunk batch version/kind",
        )?;
        require(u32_at(bytes, 56) == checksum(bytes, 56), "chunk batch CRC")?;
        let count = usize::from(u16_at(bytes, 12));
        require((1..=MAX_CHUNKS).contains(&count), "chunk descriptor count")?;
        require(
            u16_at(bytes, 14) == 0 && u32_at(bytes, 60) == 0,
            "chunk envelope reserved bytes",
        )?;
        require(
            (1..=MAX_SEGMENT).contains(&u64_at(bytes, 16)) && u64_at(bytes, 24) != 0,
            "chunk batch identity",
        )?;
        require(
            u32_at(bytes, 36) as usize == count * BLOCK_SIZE
                && u32_at(bytes, 32) as usize == (count + 1) * BLOCK_SIZE,
            "chunk batch length",
        )?;
        let first = u64_at(bytes, 40);
        require(
            first != 0 && first.checked_add(count as u64 - 1) == Some(u64_at(bytes, 48)),
            "chunk ordinal bounds",
        )?;
        let used = 64 + 64 * count;
        require(
            bytes[used..].iter().all(|&b| b == 0),
            "unused chunk descriptors",
        )?;
        for (i, slot) in bytes[64..used].as_chunks::<64>().0.iter().enumerate() {
            require(
                u32_at(slot, 32) as usize == i * BLOCK_SIZE
                    && u32_at(slot, 36) as usize == BLOCK_SIZE,
                "chunk payload coverage",
            )?;
            require(
                slot[44..].iter().all(|&b| b == 0),
                "chunk descriptor reserved bytes",
            )?;
        }
        Ok(Self { bytes, count })
    }

    pub fn segment(&self) -> u64 {
        u64_at(self.bytes, 16)
    }
    pub fn batch(&self) -> u64 {
        u64_at(self.bytes, 24)
    }
    pub fn first(&self) -> u64 {
        u64_at(self.bytes, 40)
    }
    pub fn last(&self) -> u64 {
        u64_at(self.bytes, 48)
    }
    pub fn payload_bytes(&self) -> usize {
        self.count * BLOCK_SIZE
    }
    pub fn descriptors(&self) -> impl ExactSizeIterator<Item = Descriptor> + '_ {
        self.bytes[64..64 + 64 * self.count]
            .as_chunks::<64>()
            .0
            .iter()
            .map(|slot| Descriptor::decode(slot))
    }

    pub fn verify_payload(&self, payload: &[u8]) -> io::Result<()> {
        require(
            payload.len() == self.payload_bytes(),
            "chunk payload length",
        )?;
        for descriptor in self.descriptors() {
            let start = descriptor.payload_offset as usize;
            require(
                crc32fast::hash(&payload[start..start + BLOCK_SIZE]) == descriptor.crc,
                "chunk payload CRC",
            )?;
        }
        Ok(())
    }
}

/// One bounded final allocation, containing at most 63 fixed chunks.
pub struct Builder {
    buffer: AlignedBuffer,
    count: usize,
}

impl Builder {
    pub fn new(capacity: usize) -> io::Result<Self> {
        require(
            (1..=MAX_CHUNKS).contains(&capacity),
            "chunk packing capacity",
        )?;
        Ok(Self {
            buffer: AlignedBuffer::new((capacity + 1) * BLOCK_SIZE),
            count: 0,
        })
    }

    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn allocated_bytes(&self) -> usize {
        self.buffer.as_slice().len()
    }

    pub fn push(&mut self, chunk: Chunk<'_>) -> io::Result<()> {
        require(
            (self.count + 2) * BLOCK_SIZE <= self.allocated_bytes(),
            "chunk batch full",
        )?;
        let bytes = self.buffer.as_mut_slice();
        let offset = self.count * BLOCK_SIZE;
        bytes[BLOCK_SIZE + offset..BLOCK_SIZE + offset + BLOCK_SIZE].copy_from_slice(chunk.bytes());
        let slot = &mut bytes[64 + self.count * 64..128 + self.count * 64];
        slot[..32].copy_from_slice(&chunk.hash());
        put32(slot, 32, offset as u32);
        put32(slot, 36, BLOCK_SIZE as u32);
        put32(slot, 40, crc32fast::hash(chunk.bytes()));
        self.count += 1;
        Ok(())
    }

    pub fn seal(mut self, segment: u64, batch: u64, first: u64) -> io::Result<Batch> {
        require(
            self.count != 0 && first != 0 && batch != 0 && (1..=MAX_SEGMENT).contains(&segment),
            "empty chunk batch identity",
        )?;
        let last = first
            .checked_add(self.count as u64 - 1)
            .ok_or_else(|| io::Error::other("chunk ordinals exhausted"))?;
        let bytes = &mut self.buffer.as_mut_slice()[..BLOCK_SIZE];
        bytes[..8].copy_from_slice(b"CASCHB02");
        put16(bytes, 8, 2);
        put16(bytes, 10, 1);
        put16(bytes, 12, self.count as u16);
        put64(bytes, 16, segment);
        put64(bytes, 24, batch);
        put32(bytes, 32, ((self.count + 1) * BLOCK_SIZE) as u32);
        put32(bytes, 36, (self.count * BLOCK_SIZE) as u32);
        put64(bytes, 40, first);
        put64(bytes, 48, last);
        put32(bytes, 56, checksum(bytes, 56));
        Ok(Batch {
            buffer: self.buffer,
            count: self.count,
        })
    }
}

pub struct Batch {
    buffer: AlignedBuffer,
    count: usize,
}

impl Batch {
    pub fn bytes(&self) -> &[u8] {
        &self.buffer.as_slice()[..(self.count + 1) * BLOCK_SIZE]
    }
    pub fn allocated_bytes(&self) -> usize {
        self.buffer.as_slice().len()
    }
}

#[cfg(test)]
mod tests;
