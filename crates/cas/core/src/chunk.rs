//! Shared fixed-chunk identification for the census and copying compactor.

/// Zero content is a hole; the digest of any other content is a valid chunk ID.
/// The caller determines chunk boundaries (4 KiB in the local storage profile).
pub fn nonzero_hash(bytes: &[u8]) -> Option<blake3::Hash> {
    bytes
        .iter()
        .any(|&byte| byte != 0)
        .then(|| blake3::hash(bytes))
}

/// A nonzero fixed block with its content identity computed once.
#[derive(Clone, Copy)]
pub struct Chunk<'a> {
    bytes: &'a [u8; crate::BLOCK_SIZE],
    hash: [u8; 32],
}

impl<'a> Chunk<'a> {
    pub fn new(bytes: &'a [u8; crate::BLOCK_SIZE]) -> Option<Self> {
        Some(Self {
            bytes,
            hash: *nonzero_hash(bytes)?.as_bytes(),
        })
    }
    pub fn hash(self) -> [u8; 32] {
        self.hash
    }
    pub fn bytes(self) -> &'a [u8; crate::BLOCK_SIZE] {
        self.bytes
    }
}
