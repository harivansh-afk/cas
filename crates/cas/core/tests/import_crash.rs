//! Process-exit cuts around the importer's own durable boundaries, run in a
//! separate binary like the chunk crash cuts.
#![cfg(target_os = "linux")]
mod import_support;
use cas_core::{
    catalog::Catalog,
    chunk::ChunkSize,
    import::{self, Point, image_directory},
    segments::Tickets,
    store::file::Store,
};
use import_support::*;
use std::{fs, path::Path, sync::Arc};

const BLOCKS: u64 = 2048; // 8 MiB: four compaction intervals of the test request.
const IMAGE: [u8; 16] = [7; 16];

fn zero(block: u64) -> bool {
    (300..700).contains(&block) || block.is_multiple_of(11)
}

fn child(root: &Path, size: ChunkSize, cut: &str) {
    import::run_with(
        request(root, IMAGE, &root.with_extension("raw"), Some(size)),
        memory(),
        memory(),
        |point| {
            let hit = match point {
                Point::Settled { .. } => cut == "settled",
                Point::Durable => cut == "durable",
                Point::Published => cut == "published",
            };
            if hit {
                std::process::exit(90);
            }
            Ok(())
        },
    )
    .unwrap();
    panic!("cut not reached");
}

#[test]
fn import_crash_cuts_leave_a_recoverable_root() {
    if let Some(root) = std::env::var_os("CAS_IMPORT_CRASH_ROOT") {
        child(
            Path::new(&root),
            ChunkSize::from_bytes(std::env::var("CAS_CHUNK_BYTES").unwrap().parse().unwrap())
                .unwrap(),
            &std::env::var("CAS_IMPORT_CUT").unwrap(),
        );
    }
    let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    for size in [ChunkSize::K4, ChunkSize::K16] {
        for cut in ["settled", "durable", "published"] {
            exercise(
                &dir.path().join(format!("{cut}-{}", size.bytes())),
                size,
                cut,
            );
        }
    }
}

fn exercise(root: &Path, size: ChunkSize, cut: &str) {
    fs::create_dir(root).unwrap();
    let input = root.with_extension("raw");
    let image = synthetic(&input, BLOCKS, |_| 3, zero);
    let expected = expected_chunks(&[&image], size);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "import_crash_cuts_leave_a_recoverable_root",
            "--nocapture",
        ])
        .env("CAS_IMPORT_CRASH_ROOT", root)
        .env("CAS_CHUNK_BYTES", size.bytes().to_string())
        .env("CAS_IMPORT_CUT", cut)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(90),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (listed, retained) = recovered(root);
    assert_eq!(listed, cut == "published");
    assert!(image_directory(root, IMAGE).is_dir());
    if listed {
        assert_eq!(retained, expected.len());
    } else {
        // An interrupted import keeps its output under an unreferenced image
        // name; the retry deduplicates against the retained chunks and the
        // moved WAL still pins its tickets.
        let highest = Tickets::open(root, memory()).unwrap().status().highest;
        let receipt =
            import::run(request(root, IMAGE, &input, Some(size)), memory(), memory()).unwrap();
        assert_eq!(fs::read_dir(root.join("images")).unwrap().count(), 2);
        assert!(Tickets::open(root, memory()).unwrap().status().highest > highest);
        assert!(!receipt.created_root);
        assert_eq!(receipt.store_before.chunks, retained);
        assert_eq!(receipt.store_after.chunks, expected.len());
        assert_eq!(receipt.unique_chunks_appended, expected.len() - retained);
        assert_eq!(cut == "settled", retained > 0 && retained < expected.len());
    }
    let read = read_back(root, IMAGE, image.len() as u64).unwrap();
    assert!(
        read == image,
        "chunk_bytes={} cut={cut} differs",
        size.bytes()
    );
    println!(
        "chunk_bytes={} cut={cut} listed={listed} retained_chunks={retained} verified=true",
        size.bytes()
    );
}

/// The store and catalog recover like a cold host open: whether the catalog
/// lists the image, and how many chunks the store retained.
fn recovered(root: &Path) -> (bool, usize) {
    let metadata = memory();
    let tickets = Tickets::open(root, Arc::clone(&metadata)).unwrap();
    let store = Store::inspect_recorded(Arc::clone(&tickets), Arc::clone(&metadata), memory())
        .unwrap()
        .recover()
        .unwrap();
    let catalog = Catalog::inspect(tickets, STORE, metadata).unwrap();
    (
        catalog.contents().get(IMAGE).is_some(),
        store.status().chunks,
    )
}
