//! Shared fixtures for the offline importer tests: deterministic raw images,
//! independent expected chunk sets and a cold reopen through the core read path.
#![allow(dead_code)]
use cas_core::{
    BLOCK_SIZE, MAX_REQUEST_BYTES,
    aligned::AlignedBuffer,
    append::{Limits, Log},
    budget::{Amount, Budget},
    chunk::ChunkSize,
    import::{Request, image_directory},
    manifest::file::{Identity, Manifest},
    segments::Tickets,
    store::file::Store,
};
use sha2::Digest;
use std::{collections::HashSet, fs, io, path::Path, sync::Arc};

pub const STORE: [u8; 16] = [1; 16];

pub fn memory() -> Arc<Budget> {
    Budget::new(Amount {
        bytes: 1024 * MAX_REQUEST_BYTES,
        requests: 0,
    })
}

/// Block `index` content for `seed`, or zeros where `zero` says so.
pub fn synthetic(
    path: &Path,
    blocks: u64,
    seed: impl Fn(u64) -> u64,
    zero: impl Fn(u64) -> bool,
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(blocks as usize * BLOCK_SIZE);
    for block in 0..blocks {
        if zero(block) {
            bytes.extend_from_slice(&[0; BLOCK_SIZE]);
            continue;
        }
        let mut state = seed(block).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (block + 1).wrapping_mul(0xD1B5_4A32_D192_ED03);
        for _ in 0..BLOCK_SIZE / 8 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            bytes.extend_from_slice(&state.to_le_bytes());
        }
    }
    fs::write(path, &bytes).unwrap();
    bytes
}

/// Nonzero fixed-size chunks anchored at offset zero, tail padded with zeros.
pub fn expected_chunks(images: &[&[u8]], size: ChunkSize) -> HashSet<[u8; 32]> {
    let mut set = HashSet::new();
    for image in images {
        for unit in image.chunks(size.bytes()) {
            let mut padded = vec![0; size.bytes()];
            padded[..unit.len()].copy_from_slice(unit);
            if padded.iter().any(|byte| *byte != 0) {
                set.insert(*blake3::hash(&padded).as_bytes());
            }
        }
    }
    set
}

/// Bytes the importer stages as ZERO: whole all-zero units of the chunk size,
/// counting a short tail unit at its actual length.
pub fn expected_zero_bytes(image: &[u8], size: ChunkSize) -> u64 {
    image
        .chunks(size.bytes())
        .filter(|unit| unit.iter().all(|byte| *byte == 0))
        .map(|unit| unit.len() as u64)
        .sum()
}

pub fn request<'a>(
    root: &'a Path,
    image: [u8; 16],
    input: &'a Path,
    size: Option<ChunkSize>,
) -> Request<'a> {
    Request {
        root,
        store: STORE,
        image,
        input,
        chunk_size: size,
        segment_bytes: 2 * 1024 * 1024,
        compaction_interval: 2 * 1024 * 1024,
    }
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", sha2::Sha256::digest(bytes))
}

/// Cold reopen exactly as a host would, then read every block through the
/// WAL read plan and verified store loader.
pub fn read_back(root: &Path, image: [u8; 16], image_bytes: u64) -> io::Result<Vec<u8>> {
    let metadata = memory();
    let tickets = Tickets::open(root, Arc::clone(&metadata))?;
    let inspection =
        Store::inspect_recorded(Arc::clone(&tickets), Arc::clone(&metadata), memory())?;
    let manifest = Manifest::inspect(
        &image_directory(root, image),
        Identity {
            store: STORE,
            image,
            image_bytes,
        },
        0,
        Arc::clone(&metadata),
        |hash| {
            if inspection.contains(&hash) {
                Ok(())
            } else {
                Err(io::Error::other("manifest references a missing chunk"))
            }
        },
    )?;
    let wal = Log::inspect_shared(tickets, &manifest, Limits::default(), metadata)
        .map_err(io::Error::other)?;
    let store = inspection.recover()?;
    let manifest = manifest.recover()?;
    let log = wal.fresh(manifest.view()?, 0).map_err(io::Error::other)?;
    let status = log.status();
    assert_eq!(
        (status.published, status.durable, status.compacted),
        (status.published, status.published, status.published)
    );
    let mut output = Vec::with_capacity(image_bytes as usize);
    let mut offset = 0;
    while offset < image_bytes {
        let length = (image_bytes - offset).min(MAX_REQUEST_BYTES as u64) as usize;
        let mut buffer = AlignedBuffer::new(length);
        log.read_plan(offset, length, status.published)
            .map_err(io::Error::other)?
            .read_with(&mut buffer, |hash, bytes| {
                store
                    .plan(hash)?
                    .ok_or_else(|| io::Error::other("missing chunk"))?
                    .load(bytes)
            })?;
        output.extend_from_slice(buffer.as_slice());
        offset += length as u64;
    }
    Ok(output)
}
