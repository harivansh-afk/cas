//! Isolated process: other unit tests cannot contaminate allocator/RSS samples.
#![cfg(all(target_os = "linux", target_env = "gnu"))]
use cas_core::{
    BLOCK_SIZE,
    aligned::AlignedBuffer,
    budget::{Amount, Budget},
    manifest::{
        file::{Identity, Manifest, PageCache},
        format::Extent,
        tree::LookupState,
    },
};
use std::{os::unix::fs::FileExt, sync::Arc};
#[test]
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn rss_reconciles_index_and_cache_allocations() {
    fn rss() -> usize {
        std::fs::read_to_string("/proc/self/smaps_rollup")
            .unwrap()
            .lines()
            .find_map(|l| {
                l.strip_prefix("Rss:").map(|v| {
                    v.split_whitespace()
                        .next()
                        .unwrap()
                        .parse::<usize>()
                        .unwrap()
                        * 1024
                })
            })
            .unwrap()
    }
    let metadata = Budget::new(Amount {
        bytes: 128 * 1024 * 1024,
        requests: 0,
    });
    let fixture = tempfile::tempdir().unwrap();
    let fixture_metadata = Budget::new(Amount {
        bytes: 128 * 1024 * 1024,
        requests: 0,
    });
    let mut manifest = Manifest::create(
        fixture.path(),
        Identity {
            store: [1; 16],
            image: [2; 16],
            image_bytes: BLOCK_SIZE as u64,
        },
        fixture_metadata,
    )
    .unwrap();
    let mut scratch = AlignedBuffer::new(BLOCK_SIZE);
    // Warm proc parsing before measuring an otherwise isolated process.
    let _ = rss();
    let before = rss();
    // SAFETY: mallinfo2 reads process allocator counters without input pointers.
    let allocator_before = unsafe { libc::mallinfo2() };
    let mut index = cas_core::chunk_index::Index::new(Arc::clone(&metadata));
    index.reserve(110_000).unwrap();
    for n in 0..110_000u64 {
        let hash = *blake3::hash(&n.to_le_bytes()).as_bytes();
        index
            .insert(
                hash,
                cas_core::chunk_index::Address::new(n + 1, BLOCK_SIZE as u64).unwrap(),
            )
            .unwrap();
    }
    let chunks = cas_core::cache::Cache::new(8 * 1024 * 1024, &metadata).unwrap();
    let mut bytes = [13; BLOCK_SIZE];
    for n in 0..2048u64 {
        bytes[..8].copy_from_slice(&n.to_le_bytes());
        drop(
            chunks
                .fill(*blake3::hash(&bytes).as_bytes(), &bytes)
                .unwrap(),
        );
    }
    let pages = PageCache::new(4 * 1024 * 1024, &metadata).unwrap();
    for n in 0..1024 {
        let prepared = manifest
            .prepare(
                &[Extent {
                    start: 0,
                    end: 1,
                    hash: Some([7; 32]),
                }],
                n + 1,
            )
            .unwrap();
        manifest.publish(prepared).unwrap();
        let mut read = manifest.view().unwrap().lookup(0).unwrap();
        while let LookupState::Page { offset, .. } = read.cached(&pages).unwrap() {
            read.file()
                .read_exact_at(scratch.as_mut_slice(), offset)
                .unwrap();
            read.accept_cached(offset, scratch.as_slice(), &pages)
                .unwrap();
        }
    }
    let index_bytes = index.allocated_bytes();
    let chunk_status = chunks.status();
    let page_status = pages.status();
    let charged = index_bytes + chunk_status.allocated_bytes + page_status.allocated_bytes;
    assert_eq!(
        metadata.usage().current.bytes,
        index_bytes + chunk_status.metadata_bytes + page_status.allocated_bytes
    );
    assert!(index.memory().allocator_slack_bytes < 4096);
    let after = rss();
    // SAFETY: mallinfo2 has no caller-provided pointers.
    let allocator_after = unsafe { libc::mallinfo2() };
    let free_delta = allocator_after.fordblks as i64 - allocator_before.fordblks as i64;
    let live_delta = (allocator_after.uordblks + allocator_after.hblkhd) as i64
        - (allocator_before.uordblks + allocator_before.hblkhd) as i64;
    let delta = after.saturating_sub(before);
    println!(
        "{}",
        serde_json::json!({"index_bytes":index_bytes,"chunk_cache_bytes":chunk_status.allocated_bytes,"manifest_cache_bytes":page_status.allocated_bytes,"charged_bytes":charged,"rss_before":before,"rss_after":after,"rss_delta":delta,"rss_minus_charged":delta as i64-charged as i64,"allocator_free_delta":free_delta,"allocator_live_delta":live_delta,"reconciled_residual":delta as i64-charged as i64-free_delta})
    );
    // RSS also has allocator arenas, stacks, libraries and page granularity.
    // This catches missing/duplicated tables or payloads without equating RSS
    // to an allocator's usable bytes. The raw residual is retained in evidence.
    assert!(
        (live_delta - charged as i64).unsigned_abs() < 512 * 1024,
        "allocator live {live_delta}, charged {charged}"
    );
    assert!(
        (delta as i64 - charged as i64 - free_delta).unsigned_abs() < 2 * 1024 * 1024,
        "RSS {delta}, charged {charged}, free arenas {free_delta}"
    );
    drop((index, chunks, pages));
    assert_eq!(metadata.usage().current.bytes, 0);
    println!("rss_after_drop={}", rss());
}
