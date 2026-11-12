//! Two raw images sharing half their content deduplicate across one root and
//! read back exactly through the cold host open path.
#![cfg(target_os = "linux")]
mod import_support;
use cas_core::{
    BLOCK_SIZE,
    catalog::{Catalog, Kind},
    chunk::ChunkSize,
    import::{self, image_directory},
    segments::Tickets,
};
use import_support::*;
use std::{fs, io};

const BLOCKS: u64 = 3073; // 12 MiB plus one block: a short 16 KiB tail unit.
const A: [u8; 16] = [0xa; 16];
const B: [u8; 16] = [0xb; 16];

fn zero(block: u64) -> bool {
    (100..400).contains(&block) || block.is_multiple_of(37)
}

#[test]
fn two_images_share_half_and_read_back_exactly() {
    let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let a = synthetic(&dir.path().join("a.raw"), BLOCKS, |_| 1, zero);
    let b = synthetic(
        &dir.path().join("b.raw"),
        BLOCKS,
        |block| if block < BLOCKS / 2 { 1 } else { 2 },
        zero,
    );
    let half = (BLOCKS / 2) as usize * BLOCK_SIZE;
    assert!(a[..half] == b[..half]);
    assert!(a[half..] != b[half..]);
    for size in [ChunkSize::K4, ChunkSize::K16] {
        let root = dir.path().join(format!("root-{}", size.bytes()));
        let only_a = expected_chunks(&[&a], size);
        let union = expected_chunks(&[&a, &b], size);
        assert!(only_a.len() < union.len() && union.len() < 2 * only_a.len());

        let first = import::run(
            request(&root, A, &dir.path().join("a.raw"), Some(size)),
            memory(),
            memory(),
        )
        .unwrap();
        assert!(first.created_root);
        assert_eq!(first.chunk_bytes, size.bytes());
        assert_eq!(first.image_bytes, a.len() as u64);
        assert_eq!(first.input_sha256, sha256(&a));
        assert_eq!(first.zero_bytes, expected_zero_bytes(&a, size));
        assert_eq!(first.data_bytes + first.zero_bytes, a.len() as u64);
        assert_eq!(first.unique_chunks_appended, only_a.len());
        assert_eq!(first.store_after.chunks, only_a.len());
        assert_eq!(first.store_before.chunks, 0);
        assert_eq!(first.wal.published, first.mutations);
        assert_eq!(first.wal.durable, first.mutations);
        assert_eq!(first.wal.compacted, first.mutations);
        assert!(first.settle_passes >= 6, "{}", first.settle_passes);
        assert!(first.wal_removed_segments > 0);
        assert!(first.store_after.encoded_bytes > first.store_before.encoded_bytes);
        assert!(first.store_after.index_memory.allocated_bytes > 0);

        let second = import::run(
            request(&root, B, &dir.path().join("b.raw"), None),
            memory(),
            memory(),
        )
        .unwrap();
        assert!(!second.created_root);
        assert_eq!(second.chunk_bytes, size.bytes());
        assert_eq!(second.input_sha256, sha256(&b));
        assert_eq!(second.store_before.chunks, only_a.len());
        assert_eq!(second.unique_chunks_appended, union.len() - only_a.len());
        assert_eq!(second.store_after.chunks, union.len());
        assert_eq!(second.catalog_generation, first.catalog_generation + 1);

        let catalog =
            Catalog::inspect(Tickets::open(&root, memory()).unwrap(), STORE, memory()).unwrap();
        let entries: Vec<_> = catalog.contents().entries().collect();
        assert_eq!(entries.len(), 2);
        for entry in entries {
            assert!([A, B].contains(&entry.id));
            assert_eq!(
                entry.kind,
                Kind::Image {
                    image_bytes: BLOCKS * BLOCK_SIZE as u64
                }
            );
        }
        drop(catalog);

        let read_a = read_back(&root, A, a.len() as u64).unwrap();
        let read_b = read_back(&root, B, b.len() as u64).unwrap();
        assert!(
            read_a == a,
            "image A differs after import ({})",
            size.bytes()
        );
        assert!(
            read_b == b,
            "image B differs after import ({})",
            size.bytes()
        );
        assert_eq!(sha256(&read_a), first.input_sha256);
        assert_eq!(sha256(&read_b), second.input_sha256);

        // The same ID, another store identity and an unaligned input are refused.
        let again = import::run(
            request(&root, B, &dir.path().join("b.raw"), None),
            memory(),
            memory(),
        );
        assert_eq!(again.err().unwrap().kind(), io::ErrorKind::AlreadyExists);
        let b_path = dir.path().join("b.raw");
        let mut foreign = request(&root, [0xc; 16], &b_path, None);
        foreign.store = [9; 16];
        assert!(import::run(foreign, memory(), memory()).is_err());
        assert!(!image_directory(&root, [0xc; 16]).exists());
        let odd = dir.path().join("odd.raw");
        fs::write(&odd, [1; 4097]).unwrap();
        assert!(import::run(request(&root, [0xd; 16], &odd, None), memory(), memory()).is_err());
        let other_arm = match size {
            ChunkSize::K4 => ChunkSize::K16,
            ChunkSize::K16 => ChunkSize::K4,
        };
        assert!(
            import::run(
                request(&root, [0xe; 16], &dir.path().join("a.raw"), Some(other_arm)),
                memory(),
                memory()
            )
            .is_err()
        );
        println!(
            "chunk_bytes={} unique_a={} union={} elapsed_a={:.3}s elapsed_b={:.3}s",
            size.bytes(),
            only_a.len(),
            union.len(),
            first.elapsed_seconds,
            second.elapsed_seconds
        );
    }
}
