//! Bounded version-one frames. Length includes the header, in little endian.
use cas_core::BLOCK_SIZE;
use std::io;

pub const HEADER: usize = 24;
pub const MAX_HASHES: usize = 128;
pub const GET: u8 = 1;
pub const HAS: u8 = 2;
pub const OK: u8 = 0;
pub const MISSING: u8 = 1;
pub const FAILED: u8 = 2;
pub const FRAME: usize = HEADER + BLOCK_SIZE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub op: u8,
    pub status: u8,
    pub id: u64,
    pub count: usize,
    pub bytes: usize,
}

pub fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}

impl Header {
    pub fn encode(self, out: &mut [u8]) {
        out[..HEADER].fill(0);
        out[..4].copy_from_slice(&(self.bytes as u32).to_le_bytes());
        out[4..8].copy_from_slice(b"CASP");
        out[8] = 1;
        out[9] = self.op;
        out[10] = self.status;
        out[12..20].copy_from_slice(&self.id.to_le_bytes());
        out[20..24].copy_from_slice(&(self.count as u32).to_le_bytes());
    }

    pub fn decode(input: &[u8], reply: bool) -> io::Result<Self> {
        if input.len() < HEADER || &input[4..8] != b"CASP" || input[8] != 1 || input[11] != 0 {
            return Err(invalid());
        }
        let h = Self {
            bytes: u32::from_le_bytes(input[..4].try_into().unwrap()) as usize,
            op: input[9],
            status: input[10],
            id: u64::from_le_bytes(input[12..20].try_into().unwrap()),
            count: u32::from_le_bytes(input[20..24].try_into().unwrap()) as usize,
        };
        if h.id == 0
            || !(1..=MAX_HASHES).contains(&h.count)
            || !matches!(h.op, GET | HAS)
            || (h.op == GET && h.count != 1)
            || h.status > FAILED
            || (!reply && h.status != OK)
            || (h.op == HAS && h.status == MISSING)
        {
            return Err(invalid());
        }
        let payload = if !reply {
            h.count * 32
        } else if h.status != OK {
            0
        } else if h.op == GET {
            BLOCK_SIZE
        } else {
            h.count.div_ceil(8)
        };
        if h.bytes != HEADER + payload {
            return Err(invalid());
        }
        Ok(h)
    }
}

pub fn verify(hash: &[u8; 32], bytes: &[u8]) -> io::Result<()> {
    if bytes.len() != BLOCK_SIZE || blake3::hash(bytes).as_bytes() != hash {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_rejects_unbounded_lengths_versions_and_invalid_shapes() {
        for op in [GET, HAS] {
            let h = Header {
                op,
                status: OK,
                id: 7,
                count: 1,
                bytes: HEADER + 32,
            };
            let mut bytes = [0; HEADER];
            h.encode(&mut bytes);
            assert_eq!(Header::decode(&bytes, false).unwrap(), h);
            for index in [0, 4, 8, 9, 10, 11, 20, 23] {
                let mut broken = bytes;
                broken[index] = 255;
                assert!(Header::decode(&broken, false).is_err(), "{index}");
            }
            assert!(Header::decode(&bytes[..HEADER - 1], false).is_err());
        }
    }
    #[test]
    fn every_chunk_is_verified_including_length_and_zero_content() {
        let bytes = [0; BLOCK_SIZE];
        let hash = *blake3::hash(&bytes).as_bytes();
        verify(&hash, &bytes).unwrap();
        assert!(verify(&hash, &bytes[..BLOCK_SIZE - 1]).is_err());
        assert!(verify(&hash, &[1; BLOCK_SIZE]).is_err());
    }
}
