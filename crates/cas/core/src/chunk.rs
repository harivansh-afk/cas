//! Shared fixed-chunk identification for the census and copying compactor.

/// Zero content is a hole; the digest of any other content is a valid chunk ID.
/// The caller determines chunk boundaries (4 KiB in the local storage profile).
pub fn nonzero_hash(bytes: &[u8]) -> Option<blake3::Hash> {
    bytes
        .iter()
        .any(|&byte| byte != 0)
        .then(|| blake3::hash(bytes))
}
