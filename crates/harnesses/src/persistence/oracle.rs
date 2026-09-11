//! Input operations and expected whole images; no WAL decoding.
use super::*;

#[derive(Clone, Copy, Serialize)]
pub(super) struct Operation {
    pub block: usize,
    pub blocks: usize,
    // None is a zero extent, not a stored zero payload.
    pub seed: Option<u8>,
}

impl Operation {
    fn bytes(self) -> Vec<u8> {
        (0..self.blocks * BLOCK_SIZE)
            .map(|index| {
                self.seed
                    .map_or(0, |seed| seed.wrapping_add((index % 251) as u8))
            })
            .collect()
    }
}

pub(super) struct Oracle {
    pub images: BTreeMap<u64, Vec<u8>>,
    pub operations: Vec<Operation>,
    pub sequence: u64,
    current: Vec<u8>,
}

impl Oracle {
    pub fn new() -> Self {
        Self {
            images: BTreeMap::from([(0, vec![0; IMAGE_BYTES])]),
            operations: Vec::new(),
            sequence: 0,
            current: vec![0; IMAGE_BYTES],
        }
    }

    pub fn append(&mut self, log: &mut Log, operations: &[Operation]) -> io::Result<usize> {
        let bytes = operations
            .iter()
            .filter(|op| op.seed.is_some())
            .map(|op| op.blocks * BLOCK_SIZE)
            .sum();
        let mut builder = Builder::new(IMAGE_BYTES as u64, bytes).map_err(io::Error::other)?;
        for &operation in operations {
            self.sequence += 1;
            let data = operation.bytes();
            let offset = operation.block * BLOCK_SIZE;
            self.current[offset..offset + data.len()].copy_from_slice(&data);
            self.operations.push(operation);
            let id = RequestId {
                serial: self.sequence,
                attachment: 1,
                queue: (self.sequence % 4) as u16,
                head: self.sequence as u16,
            };
            if operation.seed.is_some() {
                builder
                    .write(id, offset as u64, data.len(), |target| {
                        target.copy_from_slice(&data);
                        Ok(())
                    })
                    .map_err(io::Error::other)?;
            } else {
                builder
                    .zero(id, offset as u64, data.len() as u64)
                    .map_err(io::Error::other)?;
            }
        }
        log.append(builder).map_err(io::Error::other)?;
        // Recovery may retain a complete batch, never a proper subset of one.
        self.images.insert(self.sequence, self.current.clone());
        Ok(BLOCK_SIZE + bytes)
    }

    pub fn verify(&self, prefix: u64, required: u64, bytes: &[u8]) -> io::Result<()> {
        if prefix < required
            || self
                .images
                .get(&prefix)
                .is_none_or(|expected| expected != bytes)
        {
            return Err(io::Error::other(
                "recovered prefix or image differs from the independent oracle",
            ));
        }
        Ok(())
    }
}
