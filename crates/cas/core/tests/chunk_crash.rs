//! Process-exit cuts run in a separate binary so fork cannot transiently retain
//! file locks belonging to concurrently running storage unit tests.
#![cfg(target_os = "linux")]
use cas_core::{
    BLOCK_SIZE,
    aligned::AlignedBuffer,
    append::{
        Limits, Log, Publication,
        format::{Builder, RequestId},
    },
    budget::{Amount, Budget},
    chunk::ChunkSize,
    manifest::file::{Identity, Manifest},
    segments::Tickets,
    store::file::{Config, Store},
};
use std::{fs, io, path::Path, sync::Arc};
fn memory() -> Arc<Budget> {
    Budget::new(Amount {
        bytes: 128 * 1024 * 1024,
        requests: 0,
    })
}
fn identity() -> Identity {
    Identity {
        store: [1; 16],
        image: [2; 16],
        image_bytes: 8 * BLOCK_SIZE as u64,
    }
}
fn image(root: &Path) -> std::path::PathBuf {
    root.join("images").join("02".repeat(16))
}
fn id(n: u64) -> RequestId {
    RequestId {
        attachment: 1,
        serial: n,
        queue: 0,
        head: 0,
    }
}
fn child(root: &Path, size: ChunkSize, cut: usize) {
    let tickets = Tickets::open(root, memory()).unwrap();
    let mut store = Store::create_sized(
        tickets.clone(),
        Config {
            store: [1; 16],
            segment_bytes: 2 * 1024 * 1024,
        },
        size,
        memory(),
        memory(),
    )
    .unwrap();
    fs::create_dir_all(image(root)).unwrap();
    let mut manifest = Manifest::create_sized(&image(root), identity(), size, memory()).unwrap();
    let mut log = Log::create_shared(
        tickets,
        cas_core::append::Config {
            store: [1; 16],
            image: [2; 16],
            image_bytes: identity().image_bytes,
            segment_bytes: 2 * 1024 * 1024,
        },
        Limits::default(),
        memory(),
        manifest.view().unwrap(),
    )
    .unwrap();
    let mut batch = Builder::new(identity().image_bytes, 8 * BLOCK_SIZE).unwrap();
    batch
        .write(id(1), 0, 8 * BLOCK_SIZE, |bytes| {
            for (i, block) in bytes.chunks_mut(BLOCK_SIZE).enumerate() {
                block.fill(i as u8 + 1);
            }
            Ok(())
        })
        .unwrap();
    log.append(batch).unwrap();
    log.flush().unwrap();
    let input = log
        .select_compaction(memory(), memory())
        .unwrap()
        .unwrap()
        .load_with_store(&store.reader().unwrap())
        .unwrap();
    let receipt = input.write(&mut store, &mut manifest).unwrap();
    log.publish_compaction(receipt).unwrap();
    let reclaimed = log
        .select_reclamation(None, memory())
        .unwrap()
        .run()
        .unwrap();
    log.apply_reclamation(reclaimed).unwrap();
    let mut batch = Builder::new(identity().image_bytes, BLOCK_SIZE).unwrap();
    batch
        .write(id(2), BLOCK_SIZE as u64, BLOCK_SIZE, |bytes| {
            bytes.fill(44);
            Ok(())
        })
        .unwrap();
    batch
        .zero(id(3), 3 * BLOCK_SIZE as u64, 3 * BLOCK_SIZE as u64)
        .unwrap();
    log.append(batch).unwrap();
    log.flush().unwrap();
    let input = log
        .select_compaction(memory(), memory())
        .unwrap()
        .unwrap()
        .load_with_store(&store.reader().unwrap())
        .unwrap();
    input
        .prepare(&manifest)
        .unwrap()
        .write_with(&mut store, &mut manifest, |point, _| {
            if point
                == [
                    Publication::BeforeChunks,
                    Publication::AfterChunks,
                    Publication::AfterManifest,
                ][cut]
            {
                std::process::exit(90);
            }
            Ok(())
        })
        .unwrap();
    panic!("cut not reached");
}
#[test]
fn both_chunk_arms_process_crash_cuts() {
    if let Some(root) = std::env::var_os("CAS_CHUNK_CRASH_ROOT") {
        child(
            Path::new(&root),
            ChunkSize::from_bytes(std::env::var("CAS_CHUNK_BYTES").unwrap().parse().unwrap())
                .unwrap(),
            std::env::var("CAS_CHUNK_CUT").unwrap().parse().unwrap(),
        );
    }
    for size in [ChunkSize::K4, ChunkSize::K16] {
        for cut in 0..3 {
            let root = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "both_chunk_arms_process_crash_cuts",
                    "--nocapture",
                ])
                .env("CAS_CHUNK_CRASH_ROOT", root.path())
                .env("CAS_CHUNK_BYTES", size.bytes().to_string())
                .env("CAS_CHUNK_CUT", cut.to_string())
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(90),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let tickets = Tickets::open(root.path(), memory()).unwrap();
            let inspection = Store::inspect_recorded(tickets.clone(), memory(), memory()).unwrap();
            let manifest =
                Manifest::inspect(&image(root.path()), identity(), 0, memory(), |hash| {
                    if inspection.contains(&hash) {
                        Ok(())
                    } else {
                        Err(io::Error::other("missing crash chunk"))
                    }
                })
                .unwrap();
            let wal = Log::inspect_shared(tickets, &manifest, Limits::default(), memory()).unwrap();
            let store = inspection.recover().unwrap();
            let manifest = manifest.recover().unwrap();
            let log = wal.fresh(manifest.view().unwrap(), 3).unwrap();
            let mut bytes = AlignedBuffer::new(identity().image_bytes as usize);
            log.read_plan(0, identity().image_bytes as usize, 3)
                .unwrap()
                .read_with(&mut bytes, |hash, bytes| {
                    store.plan(hash)?.unwrap().load(bytes)
                })
                .unwrap();
            let expected = [
                vec![1; BLOCK_SIZE],
                vec![44; BLOCK_SIZE],
                vec![3; BLOCK_SIZE],
                vec![0; 3 * BLOCK_SIZE],
                vec![7; BLOCK_SIZE],
                vec![8; BLOCK_SIZE],
            ]
            .concat();
            assert!(bytes.as_slice() == expected);
            println!(
                "chunk_bytes={} cut={cut} recovered_durable=3 verified=true",
                size.bytes()
            );
        }
    }
}
