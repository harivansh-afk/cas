//! Shared fixed-chunk identification for the census and copying compactor.

/// Zero content is a hole; the digest of any other content is a valid chunk ID.
/// The caller determines chunk boundaries using the store's fixed geometry.
pub fn nonzero_hash(bytes: &[u8]) -> Option<blake3::Hash> {
    bytes
        .iter()
        .any(|&byte| byte != 0)
        .then(|| blake3::hash(bytes))
}

/// A nonzero fixed chunk with its content identity computed once.
#[derive(Clone, Copy)]
pub struct Chunk<'a> {
    bytes: &'a [u8],
    hash: [u8; 32],
}

impl<'a> Chunk<'a> {
    pub fn new(bytes: &'a [u8]) -> Option<Self> {
        assert!(
            ChunkSize::from_bytes(bytes.len()).is_ok(),
            "unsupported chunk size"
        );
        Some(Self {
            bytes,
            hash: *nonzero_hash(bytes)?.as_bytes(),
        })
    }
    pub fn hash(self) -> [u8; 32] {
        self.hash
    }
    pub fn bytes(self) -> &'a [u8] {
        self.bytes
    }
}

/// Immutable store geometry; guest IO and manifest addresses remain 4 KiB.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ChunkSize {
    #[default]
    K4,
    K16,
}
impl ChunkSize {
    pub fn from_bytes(bytes: usize) -> std::io::Result<Self> {
        match bytes {
            4096 => Ok(Self::K4),
            16384 => Ok(Self::K16),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unsupported fixed chunk size",
            )),
        }
    }
    pub const fn bytes(self) -> usize {
        match self {
            Self::K4 => 4096,
            Self::K16 => 16384,
        }
    }
    pub const fn blocks(self) -> u64 {
        (self.bytes() / crate::BLOCK_SIZE) as u64
    }
    pub fn offset(self, block: u64) -> usize {
        (block % self.blocks()) as usize * crate::BLOCK_SIZE
    }
}
