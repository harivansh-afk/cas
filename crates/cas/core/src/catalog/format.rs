use super::{Change, Entry, Id, Kind};
use crate::{
    BLOCK_SIZE,
    aligned::AlignedBuffer,
    budget::{Budget, BudgetAllocator},
    direct,
    encoding::{checksum, put16, put32, put64, require, u16_at, u32_at, u64_at},
    manifest::{file::SnapshotKey, format::Commit, format::Root},
};
use std::{fs::File, io, sync::Arc};

const HEADER: usize = 64;
const ENTRY: usize = 128;
const CRC: usize = 12;

/// Validated encoded state. Entries borrow the charged buffer and decode by
/// value; there is no separately allocated deserialization graph.
pub struct Contents {
    buffer: AlignedBuffer<BudgetAllocator>,
    metadata: Arc<Budget>,
}

impl Contents {
    pub fn store(&self) -> Id {
        id_at(self.bytes(), 16)
    }

    pub fn generation(&self) -> u64 {
        u64_at(self.bytes(), 32)
    }

    pub fn len(&self) -> usize {
        u64_at(self.bytes(), 40) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn entries(&self) -> impl ExactSizeIterator<Item = Entry> + '_ {
        self.records()
            .iter()
            .map(|record| decode(record, self.store()))
    }

    pub fn get(&self, id: Id) -> Option<Entry> {
        self.position(id)
            .ok()
            .map(|index| decode(&self.records()[index], self.store()))
    }

    pub(super) fn bytes(&self) -> &[u8] {
        self.buffer.as_slice()
    }

    fn records(&self) -> &[[u8; ENTRY]] {
        self.bytes()[HEADER..HEADER + self.len() * ENTRY]
            .as_chunks()
            .0
    }

    fn position(&self, id: Id) -> Result<usize, usize> {
        self.records()
            .binary_search_by_key(&id, |record| id_at(record, 0))
    }

    pub(super) fn empty(store: Id, metadata: Arc<Budget>) -> io::Result<Self> {
        Self::initial(store, std::iter::empty(), metadata)
    }

    pub(super) fn initial(
        store: Id,
        entries: impl ExactSizeIterator<Item = Entry>,
        metadata: Arc<Budget>,
    ) -> io::Result<Self> {
        let mut contents = Self::allocate(store, 1, entries.len(), metadata)?;
        let mut previous = [0; 16];
        for (index, entry) in entries.enumerate() {
            entry.validate(store)?;
            require(entry.id > previous, "catalog IDs are not sorted and unique")?;
            previous = entry.id;
            let offset = HEADER + index * ENTRY;
            encode(
                entry,
                &mut contents.buffer.as_mut_slice()[offset..offset + ENTRY],
            );
        }
        contents.seal();
        Ok(contents)
    }

    fn allocate(
        store: Id,
        generation: u64,
        count: usize,
        metadata: Arc<Budget>,
    ) -> io::Result<Self> {
        require(
            store != [0; 16] && generation != 0,
            "catalog identity/generation",
        )?;
        let size = encoded_size(count as u64)?;
        let mut buffer =
            AlignedBuffer::try_new_in(size, BudgetAllocator::new(Arc::clone(&metadata)))?;
        let bytes = buffer.as_mut_slice();
        bytes[..8].copy_from_slice(b"CASCAT02");
        put16(bytes, 8, 2);
        put16(bytes, 10, ENTRY as u16);
        bytes[16..32].copy_from_slice(&store);
        put64(bytes, 32, generation);
        put64(bytes, 40, count as u64);
        put64(bytes, 48, size as u64);
        Ok(Self { buffer, metadata })
    }

    pub(super) fn changed(&self, change: Change) -> io::Result<Self> {
        let (position, removed, inserted) = match change {
            Change::Insert(entry) => {
                entry.validate(self.store())?;
                let position = self.position(entry.id).err().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::AlreadyExists, "catalog ID already exists")
                })?;
                (position, 0, Some(entry))
            }
            Change::Remove(id) => {
                let position = self.position(id).map_err(|_| {
                    io::Error::new(io::ErrorKind::NotFound, "catalog ID does not exist")
                })?;
                (position, 1, None)
            }
        };
        let count = (self.len() - removed)
            .checked_add(usize::from(inserted.is_some()))
            .ok_or_else(|| io::Error::other("catalog entry count exhausted"))?;
        let generation = self
            .generation()
            .checked_add(1)
            .ok_or_else(|| io::Error::other("catalog generations exhausted"))?;
        let mut next = Self::allocate(self.store(), generation, count, Arc::clone(&self.metadata))?;
        let offset = HEADER + position * ENTRY;
        let bytes = next.buffer.as_mut_slice();
        bytes[HEADER..offset].copy_from_slice(&self.bytes()[HEADER..offset]);
        let target = offset + usize::from(inserted.is_some()) * ENTRY;
        let source = offset + removed * ENTRY;
        bytes[target..HEADER + count * ENTRY]
            .copy_from_slice(&self.bytes()[source..HEADER + self.len() * ENTRY]);
        if let Some(entry) = inserted {
            encode(entry, &mut bytes[offset..offset + ENTRY]);
        }
        next.seal();
        Ok(next)
    }

    fn seal(&mut self) {
        let crc = checksum(self.bytes(), CRC);
        put32(self.buffer.as_mut_slice(), CRC, crc);
    }

    pub(super) fn read(file: &File, store: Id, metadata: Arc<Budget>) -> io::Result<Self> {
        let length = file.metadata()?.len();
        require(length >= BLOCK_SIZE as u64, "short catalog file")?;
        let mut buffer =
            AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(Arc::clone(&metadata)))?;
        direct::read_bytes(file, buffer.as_mut_slice(), 0)?;
        let size = header(buffer.as_slice(), store)?;
        require(length == size as u64, "catalog EOF differs from header")?;
        if size > BLOCK_SIZE {
            let mut full =
                AlignedBuffer::try_new_in(size, BudgetAllocator::new(Arc::clone(&metadata)))?;
            full.as_mut_slice()[..BLOCK_SIZE].copy_from_slice(buffer.as_slice());
            direct::read_bytes(
                file,
                &mut full.as_mut_slice()[BLOCK_SIZE..],
                BLOCK_SIZE as u64,
            )?;
            buffer = full;
        }
        let contents = Self { buffer, metadata };
        contents.validate(store)?;
        Ok(contents)
    }

    fn validate(&self, store: Id) -> io::Result<()> {
        let size = header(self.bytes(), store)?;
        require(size == self.bytes().len(), "catalog buffer size")?;
        require(
            u32_at(self.bytes(), CRC) == checksum(self.bytes(), CRC),
            "catalog CRC",
        )?;
        let mut previous = [0; 16];
        for record in self.records() {
            require(matches!(u16_at(record, 16), 1 | 2), "catalog entry kind")?;
            require(
                record[18..24].iter().all(|&b| b == 0)
                    && record[82..].iter().all(|&b| b == 0)
                    && (u16_at(record, 16) == 2 || record[32..82].iter().all(|&b| b == 0)),
                "catalog entry reserved bytes",
            )?;
            let entry = decode(record, store);
            entry.validate(store)?;
            require(entry.id > previous, "catalog IDs are not sorted and unique")?;
            previous = entry.id;
        }
        require(
            self.bytes()[HEADER + self.len() * ENTRY..]
                .iter()
                .all(|&b| b == 0),
            "catalog padding",
        )
    }
}

impl Entry {
    fn validate(self, store: Id) -> io::Result<()> {
        require(self.id != [0; 16], "catalog entry ID is zero")?;
        match self.kind {
            Kind::Image { image_bytes } => Commit {
                store,
                image: self.id,
                image_bytes,
                generation: 1,
                root: Root::default(),
                durable: 0,
            }
            .validate(BLOCK_SIZE as u64),
            Kind::Snapshot(key) => {
                require(
                    key.commit.store == store,
                    "snapshot belongs to another store",
                )?;
                key.validate()
            }
        }
    }
}

fn encoded_size(count: u64) -> io::Result<usize> {
    count
        .checked_mul(ENTRY as u64)
        .and_then(|size| size.checked_add(HEADER as u64))
        .and_then(|size| size.checked_next_multiple_of(BLOCK_SIZE as u64))
        .filter(|&size| size <= i64::MAX as u64)
        .and_then(|size| usize::try_from(size).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "catalog length overflow"))
}

fn header(bytes: &[u8], store: Id) -> io::Result<usize> {
    require(bytes.len() >= HEADER, "short catalog header")?;
    require(
        &bytes[..8] == b"CASCAT02"
            && u16_at(bytes, 8) == 2
            && usize::from(u16_at(bytes, 10)) == ENTRY,
        "catalog version",
    )?;
    require(
        store != [0; 16] && id_at(bytes, 16) == store && u64_at(bytes, 32) != 0,
        "catalog identity/generation",
    )?;
    require(
        bytes[56..64].iter().all(|&b| b == 0),
        "catalog header reserved bytes",
    )?;
    let size = encoded_size(u64_at(bytes, 40))?;
    require(u64_at(bytes, 48) == size as u64, "catalog encoded length")?;
    Ok(size)
}

fn id_at(bytes: &[u8], offset: usize) -> Id {
    bytes[offset..offset + 16].try_into().unwrap()
}

fn decode(bytes: &[u8], store: Id) -> Entry {
    let image_bytes = u64_at(bytes, 24);
    Entry {
        id: id_at(bytes, 0),
        kind: if u16_at(bytes, 16) == 1 {
            Kind::Image { image_bytes }
        } else {
            Kind::Snapshot(SnapshotKey {
                commit: Commit {
                    store,
                    image: id_at(bytes, 32),
                    image_bytes,
                    generation: u64_at(bytes, 48),
                    root: Root {
                        offset: u64_at(bytes, 56),
                        height: u16_at(bytes, 80),
                    },
                    durable: u64_at(bytes, 64),
                },
                end: u64_at(bytes, 72),
            })
        },
    }
}

fn encode(entry: Entry, bytes: &mut [u8]) {
    bytes[..16].copy_from_slice(&entry.id);
    match entry.kind {
        Kind::Image { image_bytes } => {
            put16(bytes, 16, 1);
            put64(bytes, 24, image_bytes);
        }
        Kind::Snapshot(key) => {
            put16(bytes, 16, 2);
            put64(bytes, 24, key.commit.image_bytes);
            bytes[32..48].copy_from_slice(&key.commit.image);
            put64(bytes, 48, key.commit.generation);
            put64(bytes, 56, key.commit.root.offset);
            put64(bytes, 64, key.commit.durable);
            put64(bytes, 72, key.end);
            put16(bytes, 80, key.commit.root.height);
        }
    }
}

#[cfg(test)]
mod tests;
