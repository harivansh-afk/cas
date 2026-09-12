//! The v2 byte layout in docs/storage-format.md. Payload never supplies framing.
use std::io;

use crate::encoding::{checksum, put16, put32, put64, u16_at, u32_at, u64_at};

use crate::{BLOCK_SIZE, MAX_REQUEST_BYTES, aligned::AlignedBuffer};

pub const MAX_DESCRIPTORS: usize = 63;
pub const MAX_BATCH_BYTES: usize = BLOCK_SIZE + MAX_REQUEST_BYTES;
const ENVELOPE_BYTES: usize = 64;
const DESCRIPTOR_BYTES: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid v2 record: {0}")]
    Invalid(&'static str),
    #[error(transparent)]
    Io(#[from] io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;

fn require(condition: bool, message: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::Invalid(message))
    }
}

fn valid_range(offset: u64, length: u64, image_bytes: u64) -> bool {
    length != 0
        && offset.is_multiple_of(BLOCK_SIZE as u64)
        && length.is_multiple_of(BLOCK_SIZE as u64)
        && offset
            .checked_add(length)
            .is_some_and(|end| end <= image_bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentHeader {
    pub store: [u8; 16],
    pub image: [u8; 16],
    pub epoch: u64,
    pub number: u64,
    pub capacity: u64,
    pub image_bytes: u64,
    pub preceding_sequence: u64,
}

impl SegmentHeader {
    pub fn encode(self) -> Result<AlignedBuffer> {
        self.validate()?;
        let mut buffer = AlignedBuffer::new(BLOCK_SIZE);
        let bytes = buffer.as_mut_slice();
        bytes[..8].copy_from_slice(b"CASSEG02");
        put32(bytes, 8, 2);
        put32(bytes, 12, BLOCK_SIZE as u32);
        bytes[16..32].copy_from_slice(&self.store);
        bytes[32..48].copy_from_slice(&self.image);
        put64(bytes, 48, self.epoch);
        put64(bytes, 56, self.number);
        put64(bytes, 64, self.capacity);
        put64(bytes, 72, self.image_bytes);
        put64(bytes, 80, self.preceding_sequence);
        put32(bytes, 4092, checksum(bytes, 4092));
        Ok(buffer)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        require(bytes.len() == BLOCK_SIZE, "segment header length")?;
        require(
            &bytes[..8] == b"CASSEG02" && u32_at(bytes, 8) == 2,
            "segment version",
        )?;
        require(
            u32_at(bytes, 12) == BLOCK_SIZE as u32,
            "segment header size",
        )?;
        require(
            bytes[88..4092].iter().all(|byte| *byte == 0),
            "segment reserved bytes",
        )?;
        require(
            u32_at(bytes, 4092) == checksum(bytes, 4092),
            "segment checksum",
        )?;
        let value = Self {
            store: bytes[16..32].try_into().unwrap(),
            image: bytes[32..48].try_into().unwrap(),
            epoch: u64_at(bytes, 48),
            number: u64_at(bytes, 56),
            capacity: u64_at(bytes, 64),
            image_bytes: u64_at(bytes, 72),
            preceding_sequence: u64_at(bytes, 80),
        };
        value.validate()?;
        Ok(value)
    }

    pub(super) fn validate(self) -> Result<()> {
        require(
            self.store != [0; 16] && self.image != [0; 16],
            "empty identity",
        )?;
        require(
            self.epoch != 0 && self.number != 0,
            "empty epoch or segment number",
        )?;
        require(
            self.capacity >= (3 * BLOCK_SIZE) as u64
                && self.capacity.is_multiple_of(BLOCK_SIZE as u64),
            "segment capacity",
        )?;
        require(
            self.image_bytes != 0 && self.image_bytes.is_multiple_of(BLOCK_SIZE as u64),
            "image capacity",
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestId {
    pub serial: u64,
    pub attachment: u64,
    pub queue: u16,
    pub head: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Kind {
    Write = 1,
    Zero = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor {
    pub id: RequestId,
    pub sequence: u64,
    pub offset: u64,
    pub length: u64,
    pub payload_offset: u32,
    pub payload_length: u32,
    pub payload_crc: u32,
    pub kind: Kind,
}

impl Descriptor {
    fn encode(self, bytes: &mut [u8]) {
        put64(bytes, 0, self.id.serial);
        put64(bytes, 8, self.sequence);
        put64(bytes, 16, self.id.attachment);
        put64(bytes, 24, self.offset);
        put64(bytes, 32, self.length);
        put32(bytes, 40, self.payload_offset);
        put32(bytes, 44, self.payload_length);
        put32(bytes, 48, self.payload_crc);
        put16(bytes, 52, self.kind as u16);
        put16(bytes, 54, self.id.queue);
        put16(bytes, 56, self.id.head);
    }

    // Called only on fixed-size descriptor slots after checking the kind tag.
    fn decode(bytes: &[u8]) -> Self {
        Self {
            id: RequestId {
                serial: u64_at(bytes, 0),
                attachment: u64_at(bytes, 16),
                queue: u16_at(bytes, 54),
                head: u16_at(bytes, 56),
            },
            sequence: u64_at(bytes, 8),
            offset: u64_at(bytes, 24),
            length: u64_at(bytes, 32),
            payload_offset: u32_at(bytes, 40),
            payload_length: u32_at(bytes, 44),
            payload_crc: u32_at(bytes, 48),
            kind: if u16_at(bytes, 52) == 1 {
                Kind::Write
            } else {
                Kind::Zero
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Envelope {
    pub segment: u64,
    pub batch: u64,
    pub descriptors: usize,
    pub payload_bytes: usize,
    pub first: u64,
    pub last: u64,
    pub fence: bool,
}

impl Envelope {
    fn encode(self, bytes: &mut [u8]) {
        bytes[..8].copy_from_slice(b"CASBAT02");
        put16(bytes, 8, 2);
        put16(bytes, 10, if self.fence { 2 } else { 1 });
        put16(bytes, 12, self.descriptors as u16);
        put64(bytes, 16, self.segment);
        put64(bytes, 24, self.batch);
        put32(bytes, 32, (BLOCK_SIZE + self.payload_bytes) as u32);
        put32(bytes, 36, self.payload_bytes as u32);
        put64(bytes, 40, self.first);
        put64(bytes, 48, self.last);
        put32(bytes, 56, checksum(bytes, 56));
    }
}

pub struct Header<'a> {
    envelope: Envelope,
    bytes: &'a [u8],
}

impl<'a> Header<'a> {
    pub fn envelope(&self) -> Envelope {
        self.envelope
    }

    pub fn decode(bytes: &'a [u8], image_bytes: u64) -> Result<Self> {
        require(bytes.len() == BLOCK_SIZE, "batch header length")?;
        require(
            &bytes[..8] == b"CASBAT02" && u16_at(bytes, 8) == 2,
            "batch version",
        )?;
        require(matches!(u16_at(bytes, 10), 1 | 2), "batch kind")?;
        require(u32_at(bytes, 56) == checksum(bytes, 56), "batch checksum")?;
        let envelope = Envelope {
            segment: u64_at(bytes, 16),
            batch: u64_at(bytes, 24),
            descriptors: u16_at(bytes, 12) as usize,
            payload_bytes: u32_at(bytes, 36) as usize,
            first: u64_at(bytes, 40),
            last: u64_at(bytes, 48),
            fence: u16_at(bytes, 10) == 2,
        };
        require(
            envelope.segment != 0 && envelope.batch != 0,
            "batch identity",
        )?;
        require(envelope.descriptors <= MAX_DESCRIPTORS, "descriptor count")?;
        require(
            envelope.payload_bytes <= MAX_REQUEST_BYTES
                && envelope.payload_bytes.is_multiple_of(BLOCK_SIZE),
            "payload length",
        )?;
        require(
            u32_at(bytes, 32) as usize == BLOCK_SIZE + envelope.payload_bytes,
            "batch length",
        )?;
        require(
            u16_at(bytes, 14) == 0 && u32_at(bytes, 60) == 0,
            "envelope reserved bytes",
        )?;
        let used = ENVELOPE_BYTES + DESCRIPTOR_BYTES * envelope.descriptors;
        require(
            bytes[used..].iter().all(|byte| *byte == 0),
            "unused descriptor bytes",
        )?;
        if envelope.fence {
            require(
                envelope.descriptors == 0
                    && envelope.payload_bytes == 0
                    && envelope.first == envelope.last,
                "fence contents",
            )?;
        } else {
            require(
                envelope.descriptors != 0
                    && envelope.first != 0
                    && envelope.first.checked_add(envelope.descriptors as u64 - 1)
                        == Some(envelope.last),
                "mutation bounds",
            )?;
        }
        let header = Self { envelope, bytes };
        let mut payload_end = 0u64;
        let mut previous_serial = 0;
        for (index, slot) in bytes[ENVELOPE_BYTES..used]
            .as_chunks::<DESCRIPTOR_BYTES>()
            .0
            .iter()
            .enumerate()
        {
            require(matches!(u16_at(slot, 52), 1 | 2), "mutation kind")?;
            require(
                slot[58..].iter().all(|byte| *byte == 0),
                "descriptor reserved bytes",
            )?;
            let descriptor = Descriptor::decode(slot);
            require(
                descriptor.sequence == envelope.first + index as u64,
                "mutation sequence",
            )?;
            require(
                descriptor.id.serial > previous_serial && descriptor.id.attachment != 0,
                "request identity",
            )?;
            previous_serial = descriptor.id.serial;
            require(
                valid_range(descriptor.offset, descriptor.length, image_bytes),
                "logical range",
            )?;
            match descriptor.kind {
                Kind::Write => {
                    require(
                        descriptor.length == u64::from(descriptor.payload_length)
                            && u64::from(descriptor.payload_offset) == payload_end,
                        "WRITE payload range",
                    )?;
                    payload_end += u64::from(descriptor.payload_length);
                    require(
                        payload_end <= envelope.payload_bytes as u64,
                        "payload exceeds batch",
                    )?;
                }
                Kind::Zero => require(
                    descriptor.payload_offset == 0
                        && descriptor.payload_length == 0
                        && descriptor.payload_crc == 0,
                    "ZERO payload",
                )?,
            }
        }
        require(
            payload_end == envelope.payload_bytes as u64,
            "payload coverage",
        )?;
        Ok(header)
    }

    pub(crate) fn follows(
        &self,
        segment: SegmentHeader,
        batch: u64,
        preceding: u64,
        offset: u64,
        end: u64,
    ) -> bool {
        let envelope = self.envelope();
        let length = (BLOCK_SIZE + envelope.payload_bytes) as u64;
        envelope.segment == segment.number
            && envelope.batch == batch
            && offset.checked_add(length).is_some_and(|after| {
                after <= end
                    && after <= segment.capacity
                    && (envelope.fence
                        || after
                            .checked_add(BLOCK_SIZE as u64)
                            .is_some_and(|fenced| fenced <= segment.capacity))
            })
            && if envelope.fence {
                envelope.last == preceding
            } else {
                preceding.checked_add(1) == Some(envelope.first)
            }
    }

    pub fn descriptors(&self) -> impl Iterator<Item = Descriptor> + '_ {
        self.bytes[ENVELOPE_BYTES..ENVELOPE_BYTES + self.envelope.descriptors * DESCRIPTOR_BYTES]
            .as_chunks::<DESCRIPTOR_BYTES>()
            .0
            .iter()
            .map(|slot| Descriptor::decode(slot))
    }

    pub fn verify_payload(&self, payload: &[u8]) -> Result<()> {
        require(
            payload.len() == self.envelope.payload_bytes,
            "short batch payload",
        )?;
        for descriptor in self.descriptors().filter(|item| item.kind == Kind::Write) {
            let start = descriptor.payload_offset as usize;
            let end = start + descriptor.payload_length as usize;
            require(
                crc32fast::hash(&payload[start..end]) == descriptor.payload_crc,
                "payload checksum",
            )?;
        }
        Ok(())
    }
}

/// The allocation is final before the first payload byte is gathered.
pub struct Builder {
    buffer: AlignedBuffer,
    image_bytes: u64,
    descriptors: usize,
    payload_bytes: usize,
    last_serial: u64,
}

impl Builder {
    pub fn new(image_bytes: u64, payload_capacity: usize) -> Result<Self> {
        require(
            image_bytes != 0 && image_bytes.is_multiple_of(BLOCK_SIZE as u64),
            "image capacity",
        )?;
        require(
            payload_capacity <= MAX_REQUEST_BYTES && payload_capacity.is_multiple_of(BLOCK_SIZE),
            "packing capacity",
        )?;
        Ok(Self {
            buffer: AlignedBuffer::new(BLOCK_SIZE + payload_capacity),
            image_bytes,
            descriptors: 0,
            payload_bytes: 0,
            last_serial: 0,
        })
    }

    pub fn allocation_bytes(&self) -> usize {
        self.buffer.as_slice().len()
    }
    pub fn image_bytes(&self) -> u64 {
        self.image_bytes
    }
    pub fn allocation_address(&self) -> usize {
        self.buffer.as_slice().as_ptr() as usize
    }
    pub fn len(&self) -> usize {
        self.descriptors
    }
    pub fn is_empty(&self) -> bool {
        self.descriptors == 0
    }
    pub fn payload_bytes(&self) -> usize {
        self.payload_bytes
    }

    fn admit(&self, id: RequestId, offset: u64, length: u64, payload: usize) -> Result<()> {
        require(self.descriptors < MAX_DESCRIPTORS, "full descriptor batch")?;
        require(
            id.serial > self.last_serial && id.attachment != 0,
            "request identity",
        )?;
        require(
            valid_range(offset, length, self.image_bytes),
            "logical range",
        )?;
        require(
            payload <= self.allocation_bytes() - BLOCK_SIZE - self.payload_bytes,
            "full payload batch",
        )
    }

    pub fn write(
        &mut self,
        id: RequestId,
        offset: u64,
        length: usize,
        gather: impl FnOnce(&mut [u8]) -> io::Result<()>,
    ) -> Result<()> {
        self.admit(id, offset, length as u64, length)?;
        let start = BLOCK_SIZE + self.payload_bytes;
        let destination = &mut self.buffer.as_mut_slice()[start..start + length];
        gather(destination)?;
        let descriptor = Descriptor {
            id,
            sequence: 0,
            offset,
            length: length as u64,
            payload_offset: self.payload_bytes as u32,
            payload_length: length as u32,
            payload_crc: crc32fast::hash(destination),
            kind: Kind::Write,
        };
        self.push(descriptor);
        self.payload_bytes += length;
        Ok(())
    }

    pub fn zero(&mut self, id: RequestId, offset: u64, length: u64) -> Result<()> {
        self.admit(id, offset, length, 0)?;
        self.push(Descriptor {
            id,
            sequence: 0,
            offset,
            length,
            payload_offset: 0,
            payload_length: 0,
            payload_crc: 0,
            kind: Kind::Zero,
        });
        Ok(())
    }

    fn push(&mut self, descriptor: Descriptor) {
        let start = ENVELOPE_BYTES + self.descriptors * DESCRIPTOR_BYTES;
        descriptor.encode(&mut self.buffer.as_mut_slice()[start..start + DESCRIPTOR_BYTES]);
        self.descriptors += 1;
        self.last_serial = descriptor.id.serial;
    }

    /// The image sequencer assigns dense mutations and physical framing together.
    pub fn seal(mut self, segment: u64, batch: u64, first: u64) -> Result<Batch> {
        require(
            !self.is_empty() && first != 0 && segment != 0 && batch != 0,
            "empty batch identity",
        )?;
        let last = first
            .checked_add(self.descriptors as u64 - 1)
            .ok_or(Error::Invalid("sequence exhausted"))?;
        for index in 0..self.descriptors {
            put64(
                self.buffer.as_mut_slice(),
                ENVELOPE_BYTES + index * DESCRIPTOR_BYTES + 8,
                first + index as u64,
            );
        }
        let envelope = Envelope {
            segment,
            batch,
            descriptors: self.descriptors,
            payload_bytes: self.payload_bytes,
            first,
            last,
            fence: false,
        };
        envelope.encode(&mut self.buffer.as_mut_slice()[..BLOCK_SIZE]);
        Ok(Batch {
            buffer: self.buffer,
            envelope,
        })
    }
}

pub struct Batch {
    buffer: AlignedBuffer,
    envelope: Envelope,
}

impl Batch {
    pub fn envelope(&self) -> Envelope {
        self.envelope
    }

    pub fn fence(segment: u64, batch: u64, boundary: u64) -> Result<Self> {
        require(segment != 0 && batch != 0, "fence identity")?;
        let mut buffer = AlignedBuffer::new(BLOCK_SIZE);
        let envelope = Envelope {
            segment,
            batch,
            descriptors: 0,
            payload_bytes: 0,
            first: boundary,
            last: boundary,
            fence: true,
        };
        envelope.encode(buffer.as_mut_slice());
        Ok(Self { buffer, envelope })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.buffer.as_slice()[..BLOCK_SIZE + self.envelope.payload_bytes]
    }
    pub fn allocation_bytes(&self) -> usize {
        self.buffer.as_slice().len()
    }
    pub fn allocation_address(&self) -> usize {
        self.buffer.as_slice().as_ptr() as usize
    }
}

#[cfg(test)]
mod tests;
