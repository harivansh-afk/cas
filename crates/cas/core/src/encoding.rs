//! Little-endian fields in already length-checked fixed-size storage records.
pub(crate) fn require(condition: bool, message: &'static str) -> std::io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message,
        ))
    }
}

pub(crate) fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}
pub(crate) fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
pub(crate) fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
pub(crate) fn put16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}
pub(crate) fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
pub(crate) fn put64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn checksum(bytes: &[u8], field: usize) -> u32 {
    let mut crc = crc32fast::Hasher::new();
    crc.update(&bytes[..field]);
    crc.update(&[0; 4]);
    crc.update(&bytes[field + 4..]);
    crc.finalize()
}
