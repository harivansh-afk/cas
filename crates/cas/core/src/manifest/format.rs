//! Bounded 4 KiB tree and COMMIT pages; no payload scanning or native structs.
use crate::{
    BLOCK_SIZE,
    aligned::AlignedBuffer,
    chunk_index::Hash,
    encoding::{checksum, put16, put32, put64, require, u16_at, u32_at, u64_at},
};
use std::io;

pub const LEAF_CAPACITY: usize = 63;
pub const BRANCH_CAPACITY: usize = 252;
pub const MAX_HEIGHT: u16 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Kind {
    Leaf = 1,
    Branch = 2,
    Commit = 3,
    File = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    pub start: u64,
    pub end: u64,
    /// None denotes ZERO. All bit patterns, including an all-zero ID, are hashes.
    pub hash: Option<Hash>,
}

impl Extent {
    fn decode(bytes: &[u8]) -> Self {
        Self {
            start: u64_at(bytes, 0),
            end: u64_at(bytes, 8),
            hash: (u16_at(bytes, 52) == 1).then(|| bytes[16..48].try_into().unwrap()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Child {
    pub start: u64,
    pub offset: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Root {
    pub offset: u64,
    pub height: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Commit {
    pub store: [u8; 16],
    pub image: [u8; 16],
    pub generation: u64,
    pub root: Root,
    pub durable: u64,
    pub image_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileHeader {
    pub store: [u8; 16],
    pub image_bytes: u64,
}

fn page_offset(offset: u64) -> bool {
    offset != 0 && offset.is_multiple_of(BLOCK_SIZE as u64)
}

pub struct Page<'a> {
    bytes: &'a [u8],
    kind: Kind,
    count: usize,
}

impl<'a> Page<'a> {
    pub fn decode(bytes: &'a [u8], offset: u64, image_bytes: u64) -> io::Result<Self> {
        require(bytes.len() == BLOCK_SIZE, "manifest page size")?;
        require(
            &bytes[..8] == b"CASMAN02" && u16_at(bytes, 8) == 2,
            "manifest page version",
        )?;
        require(
            u32_at(bytes, 56) == checksum(bytes, 56),
            "manifest page CRC",
        )?;
        require(
            u64_at(bytes, 16) == offset && offset.is_multiple_of(BLOCK_SIZE as u64),
            "manifest page address",
        )?;
        require(
            image_bytes != 0 && image_bytes.is_multiple_of(BLOCK_SIZE as u64),
            "manifest image capacity",
        )?;
        require(
            bytes[24..56].iter().all(|&b| b == 0) && u32_at(bytes, 60) == 0,
            "manifest reserved header",
        )?;
        let kind = match u16_at(bytes, 10) {
            1 => Kind::Leaf,
            2 => Kind::Branch,
            3 => Kind::Commit,
            4 => Kind::File,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "manifest page kind",
                ));
            }
        };
        require(
            (kind == Kind::File) == (offset == 0),
            "manifest file/page address",
        )?;
        let page = Self {
            bytes,
            kind,
            count: usize::from(u16_at(bytes, 14)),
        };
        let blocks = image_bytes / BLOCK_SIZE as u64;
        match kind {
            Kind::Leaf => page.validate_leaf(blocks)?,
            Kind::Branch => page.validate_branch(blocks)?,
            Kind::File => {
                require(page.level() == 0 && page.count == 0, "manifest FILE shape")?;
                require(
                    bytes[64..80] != [0; 16] && u64_at(bytes, 80) == image_bytes,
                    "manifest FILE identity/capacity",
                )?;
                require(
                    bytes[88..].iter().all(|&b| b == 0),
                    "manifest FILE reserved bytes",
                )?;
            }
            Kind::Commit => {
                let commit = page.commit()?;
                require(
                    page.count == 0
                        && commit.store != [0; 16]
                        && commit.image != [0; 16]
                        && commit.generation != 0
                        && commit.image_bytes == image_bytes,
                    "manifest COMMIT identity/capacity",
                )?;
                let root = commit.root;
                require(
                    if root.offset == 0 {
                        root.height == 0
                    } else {
                        page_offset(root.offset)
                            && root.offset < offset
                            && (1..=MAX_HEIGHT).contains(&root.height)
                    },
                    "manifest COMMIT root",
                )?;
                require(
                    bytes[128..].iter().all(|&b| b == 0),
                    "manifest COMMIT reserved bytes",
                )?;
            }
        }
        Ok(page)
    }

    fn validate_leaf(&self, blocks: u64) -> io::Result<()> {
        require(
            self.level() == 0 && (1..=LEAF_CAPACITY).contains(&self.count),
            "manifest leaf shape",
        )?;
        let used = 64 + self.count * 64;
        require(
            self.bytes[used..].iter().all(|&b| b == 0),
            "unused manifest leaf entries",
        )?;
        let mut end = 0;
        for slot in self.bytes[64..used].as_chunks::<64>().0 {
            let extent = Extent::decode(slot);
            require(
                extent.start >= end && extent.start < extent.end && extent.end <= blocks,
                "manifest extent bounds/order",
            )?;
            require(
                u32_at(slot, 48) == 0 && slot[54..].iter().all(|&b| b == 0),
                "manifest extent offset/reserved",
            )?;
            match u16_at(slot, 52) {
                1 => require(extent.end - extent.start == 1, "fixed chunk extent length")?,
                2 => require(slot[16..48] == [0; 32], "ZERO extent hash")?,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "manifest extent kind",
                    ));
                }
            }
            end = extent.end;
        }
        Ok(())
    }

    fn validate_branch(&self, blocks: u64) -> io::Result<()> {
        require(
            (1..MAX_HEIGHT).contains(&self.level()) && (1..=BRANCH_CAPACITY).contains(&self.count),
            "manifest branch shape",
        )?;
        let used = 64 + self.count * 16;
        require(
            self.bytes[used..].iter().all(|&b| b == 0),
            "unused manifest children",
        )?;
        let mut previous = None;
        for slot in self.bytes[64..used].as_chunks::<16>().0 {
            let start = u64_at(slot, 0);
            let child = u64_at(slot, 8);
            require(
                start < blocks && previous.is_none_or(|value| start > value),
                "manifest child order",
            )?;
            require(
                page_offset(child) && child < self.offset(),
                "manifest child address",
            )?;
            previous = Some(start);
        }
        Ok(())
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }
    pub fn offset(&self) -> u64 {
        u64_at(self.bytes, 16)
    }
    pub fn level(&self) -> u16 {
        u16_at(self.bytes, 12)
    }

    pub fn extents(&self) -> io::Result<impl ExactSizeIterator<Item = Extent> + '_> {
        require(self.kind == Kind::Leaf, "expected manifest leaf")?;
        Ok(self.bytes[64..64 + 64 * self.count]
            .as_chunks::<64>()
            .0
            .iter()
            .map(|slot| Extent::decode(slot)))
    }

    pub fn children(&self) -> io::Result<impl ExactSizeIterator<Item = Child> + '_> {
        require(self.kind == Kind::Branch, "expected manifest branch")?;
        Ok(self.bytes[64..64 + 16 * self.count]
            .as_chunks::<16>()
            .0
            .iter()
            .map(|slot| Child {
                start: u64_at(slot, 0),
                offset: u64_at(slot, 8),
            }))
    }

    pub fn commit(&self) -> io::Result<Commit> {
        require(self.kind == Kind::Commit, "expected manifest COMMIT")?;
        Ok(Commit {
            store: self.bytes[64..80].try_into().unwrap(),
            image: self.bytes[80..96].try_into().unwrap(),
            generation: u64_at(self.bytes, 96),
            root: Root {
                offset: u64_at(self.bytes, 104),
                height: self.level(),
            },
            durable: u64_at(self.bytes, 112),
            image_bytes: u64_at(self.bytes, 120),
        })
    }
}

fn start(kind: Kind, offset: u64, level: u16, count: usize) -> AlignedBuffer {
    let mut buffer = AlignedBuffer::new(BLOCK_SIZE);
    let bytes = buffer.as_mut_slice();
    bytes[..8].copy_from_slice(b"CASMAN02");
    put16(bytes, 8, 2);
    put16(bytes, 10, kind as u16);
    put16(bytes, 12, level);
    put16(bytes, 14, count as u16);
    put64(bytes, 16, offset);
    buffer
}

fn finish(mut buffer: AlignedBuffer, offset: u64, image_bytes: u64) -> io::Result<AlignedBuffer> {
    let bytes = buffer.as_mut_slice();
    put32(bytes, 56, checksum(bytes, 56));
    Page::decode(bytes, offset, image_bytes)?;
    Ok(buffer)
}

pub fn leaf(offset: u64, image_bytes: u64, extents: &[Extent]) -> io::Result<AlignedBuffer> {
    require(extents.len() <= LEAF_CAPACITY, "manifest leaf capacity")?;
    let mut buffer = start(Kind::Leaf, offset, 0, extents.len());
    for (extent, slot) in extents
        .iter()
        .zip(buffer.as_mut_slice()[64..].as_chunks_mut::<64>().0)
    {
        put64(slot, 0, extent.start);
        put64(slot, 8, extent.end);
        if let Some(hash) = extent.hash {
            slot[16..48].copy_from_slice(&hash);
        }
        put16(slot, 52, if extent.hash.is_some() { 1 } else { 2 });
    }
    finish(buffer, offset, image_bytes)
}

pub fn branch(
    offset: u64,
    image_bytes: u64,
    level: u16,
    children: &[Child],
) -> io::Result<AlignedBuffer> {
    require(
        children.len() <= BRANCH_CAPACITY,
        "manifest branch capacity",
    )?;
    let mut buffer = start(Kind::Branch, offset, level, children.len());
    for (child, slot) in children
        .iter()
        .zip(buffer.as_mut_slice()[64..].as_chunks_mut::<16>().0)
    {
        put64(slot, 0, child.start);
        put64(slot, 8, child.offset);
    }
    finish(buffer, offset, image_bytes)
}

impl FileHeader {
    pub fn encode(self) -> io::Result<AlignedBuffer> {
        let mut buffer = start(Kind::File, 0, 0, 0);
        buffer.as_mut_slice()[64..80].copy_from_slice(&self.store);
        put64(buffer.as_mut_slice(), 80, self.image_bytes);
        finish(buffer, 0, self.image_bytes)
    }

    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        require(bytes.len() == BLOCK_SIZE, "manifest FILE length")?;
        let image_bytes = u64_at(bytes, 80);
        let page = Page::decode(bytes, 0, image_bytes)?;
        require(page.kind() == Kind::File, "expected manifest FILE")?;
        Ok(Self {
            store: bytes[64..80].try_into().unwrap(),
            image_bytes,
        })
    }
}

impl Commit {
    pub fn encode(self, offset: u64) -> io::Result<AlignedBuffer> {
        let mut buffer = start(Kind::Commit, offset, self.root.height, 0);
        let bytes = buffer.as_mut_slice();
        bytes[64..80].copy_from_slice(&self.store);
        bytes[80..96].copy_from_slice(&self.image);
        put64(bytes, 96, self.generation);
        put64(bytes, 104, self.root.offset);
        put64(bytes, 112, self.durable);
        put64(bytes, 120, self.image_bytes);
        finish(buffer, offset, self.image_bytes)
    }
}

#[cfg(test)]
mod tests;
