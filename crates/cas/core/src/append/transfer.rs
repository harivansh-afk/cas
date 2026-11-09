//! A frozen, durable tail exported as logical mutations. Receivers allocate
//! their own segment tickets and attachment epoch; image sequence numbers stay.
use super::*;
use crate::{direct, encoding::require};
use std::io::{Read, Write};

pub struct Transfer {
    spans: BudgetVec<compaction::Span, BudgetAllocator>,
    base: u64,
    through: u64,
    image_bytes: u64,
    memory: Arc<Budget>,
}
impl Log {
    pub fn capture_transfer(&self, memory: Arc<Budget>) -> Result<Transfer> {
        self.drained()?;
        require(
            self.covers_flush(self.published),
            "transfer requires a fenced published cut",
        )?;
        let base = self
            .base
            .as_ref()
            .ok_or_else(|| io::Error::other("transfer requires a manifest"))?
            .commit()
            .durable;
        Ok(Transfer {
            spans: compaction::spans(self, memory.clone(), true)?,
            base,
            through: self.durable,
            image_bytes: self.config.image_bytes,
            memory,
        })
    }
    /// The caller owns an unpublished destination image directory and validates
    /// every manifest dependency before creating this new local WAL namespace.
    pub fn create_transferred(
        tickets: Arc<crate::segments::Tickets>,
        config: Config,
        limits: Limits,
        metadata: Arc<Budget>,
        base: crate::manifest::file::View,
    ) -> Result<Self> {
        require(
            config.store == base.commit().store
                && config.image == base.commit().image
                && config.image_bytes == base.commit().image_bytes,
            "transferred WAL identity",
        )?;
        let path = tickets
            .root()
            .join("images")
            .join(format!("{:032x}", u128::from_be_bytes(config.image)))
            .join("staging");
        Self::create_in(&path, config, limits, metadata, Some(tickets), Some(base))
    }
    pub fn import_transfer(&mut self, input: &mut impl Read, required: u64) -> Result<()> {
        self.drained()?;
        let base = self
            .base
            .as_ref()
            .ok_or_else(|| io::Error::other("transfer requires a manifest"))?
            .commit()
            .durable;
        require(
            self.published == base && self.index.len() == 0,
            "transfer destination is not empty",
        )?;
        let mut header = [0; 24];
        input.read_exact(&mut header)?;
        require(
            &header[..8] == b"CASTAIL1"
                && u64::from_le_bytes(header[8..16].try_into().unwrap()) == base
                && u64::from_le_bytes(header[16..24].try_into().unwrap()) == required
                && required >= base,
            "transferred tail cut",
        )?;
        let mut builder = Builder::new(self.config.image_bytes, MAX_REQUEST_BYTES)?;
        for sequence in (base..required).map(|value| value + 1) {
            let mut record = [0; 32];
            input.read_exact(&mut record)?;
            let actual = u64::from_le_bytes(record[..8].try_into().unwrap());
            let offset = u64::from_le_bytes(record[8..16].try_into().unwrap());
            let length = u64::from_le_bytes(record[16..24].try_into().unwrap());
            require(
                actual == sequence
                    && matches!(record[24], 1 | 2)
                    && record[25..] == [0; 7]
                    && length != 0,
                "transferred mutation identity",
            )?;
            let payload = if record[24] == 1 {
                usize::try_from(length)
                    .map_err(|_| io::Error::other("transfer payload overflow"))?
            } else {
                0
            };
            require(payload <= MAX_REQUEST_BYTES, "transfer payload bound")?;
            if builder.len() == format::MAX_DESCRIPTORS
                || builder.payload_bytes() + payload > MAX_REQUEST_BYTES
            {
                let old = std::mem::replace(
                    &mut builder,
                    Builder::new(self.config.image_bytes, MAX_REQUEST_BYTES)?,
                );
                self.append(old)?;
            }
            let id = format::RequestId {
                attachment: self.status().epoch,
                serial: sequence,
                queue: 0,
                head: 0,
            };
            if record[24] == 1 {
                builder.write(id, offset, payload, |out| input.read_exact(out))?;
            } else {
                builder.zero(id, offset, length)?;
            }
        }
        if !builder.is_empty() {
            self.append(builder)?;
        }
        require(
            input.read(&mut [0; 1])? == 0,
            "transferred tail trailing data",
        )?;
        require(
            self.published == required,
            "transferred tail ended below required E",
        )?;
        self.flush()?;
        Ok(())
    }
}
impl Transfer {
    pub fn write(&self, output: &mut impl Write) -> io::Result<()> {
        output.write_all(b"CASTAIL1")?;
        output.write_all(&self.base.to_le_bytes())?;
        output.write_all(&self.through.to_le_bytes())?;
        let mut page =
            AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(self.memory.clone()))?;
        let mut payload = AlignedBuffer::try_new_in(
            MAX_REQUEST_BYTES,
            BudgetAllocator::new(self.memory.clone()),
        )?;
        let first = self
            .spans
            .first()
            .ok_or_else(|| io::Error::other("transfer has no WAL"))?;
        let mut sequence = first.segment.header.preceding_sequence;
        require(sequence <= self.base, "transfer WAL starts above D")?;
        'segments: for span in &self.spans {
            require(
                span.segment.header.preceding_sequence == sequence,
                "transfer segment gap",
            )?;
            let mut offset = BLOCK_SIZE as u64;
            let mut batch = 1;
            while offset < span.end {
                if sequence == self.through {
                    break 'segments;
                }
                direct::read_bytes(&span.segment.file, page.as_mut_slice(), offset)?;
                let header = format::Header::decode(page.as_slice(), self.image_bytes)
                    .map_err(io::Error::other)?;
                require(
                    header.follows(span.segment.header, batch, sequence, offset, span.end),
                    "transfer WAL batch order",
                )?;
                let envelope = header.envelope();
                if !envelope.fence {
                    if envelope.last > self.base {
                        require(
                            envelope.first > self.base && envelope.last <= self.through,
                            "transfer cut splits a batch",
                        )?;
                        let data = &mut payload.as_mut_slice()[..envelope.payload_bytes];
                        if !data.is_empty() {
                            direct::read_bytes(
                                &span.segment.file,
                                data,
                                offset + BLOCK_SIZE as u64,
                            )?;
                        }
                        header.verify_payload(data).map_err(io::Error::other)?;
                        for descriptor in header.descriptors() {
                            let mut record = [0; 32];
                            record[..8].copy_from_slice(&descriptor.sequence.to_le_bytes());
                            record[8..16].copy_from_slice(&descriptor.offset.to_le_bytes());
                            record[16..24].copy_from_slice(&descriptor.length.to_le_bytes());
                            record[24] = match descriptor.kind {
                                format::Kind::Write => 1,
                                format::Kind::Zero => 2,
                            };
                            output.write_all(&record)?;
                            if descriptor.kind == format::Kind::Write {
                                let start = descriptor.payload_offset as usize;
                                output.write_all(
                                    &data[start..start + descriptor.payload_length as usize],
                                )?;
                            }
                        }
                    }
                    sequence = envelope.last;
                }
                offset += (BLOCK_SIZE + envelope.payload_bytes) as u64;
                batch += 1;
            }
        }
        require(sequence == self.through, "transfer scan ended below E")
    }
}
