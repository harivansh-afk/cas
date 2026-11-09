use super::*;
use crate::chunk::ChunkSize;
fn config() -> Config {
    Config {
        segment_bytes: 2 * 1024 * 1024,
        ..CONFIG
    }
}
fn open(path: &Path) -> Store {
    Store::inspect_sized(
        Tickets::open(path, metadata()).unwrap(),
        config(),
        ChunkSize::K16,
        metadata(),
        metadata(),
    )
    .unwrap()
    .recover()
    .unwrap()
}
#[test]
fn geometry_is_durable_for_empty_stores_and_mixed_chunks_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::create_sized(
        Tickets::open(root.path(), metadata()).unwrap(),
        config(),
        ChunkSize::K16,
        metadata(),
        metadata(),
    )
    .unwrap();
    assert!(
        store
            .insert(&[Chunk::new(&[1; BLOCK_SIZE]).unwrap()])
            .is_err()
    );
    assert!(!store.status().failed);
    drop(store);
    assert!(
        Store::inspect_sized(
            Tickets::open(root.path(), metadata()).unwrap(),
            config(),
            ChunkSize::K4,
            metadata(),
            metadata()
        )
        .is_err()
    );
    let mut store = open(root.path());
    for n in 1..=130u64 {
        let mut bytes = [7; 16384];
        bytes[..8].copy_from_slice(&n.to_le_bytes());
        store.insert(&[Chunk::new(&bytes).unwrap()]).unwrap();
    }
    drop(store);
    let store = open(root.path());
    assert_eq!(store.status().chunks, 130);
    let mut out = AlignedBuffer::new(16384);
    for n in 1..=130u64 {
        let mut bytes = [7; 16384];
        bytes[..8].copy_from_slice(&n.to_le_bytes());
        store
            .plan(Chunk::new(&bytes).unwrap().hash())
            .unwrap()
            .unwrap()
            .load(out.as_mut_slice())
            .unwrap();
        assert!(out.as_slice() == bytes);
    }
}
#[test]
fn collection_faults_preserve_16k_chunks_through_full_store_recovery() {
    for (fault, after) in [
        (Fault::Read, 0),
        (Fault::Read, 2),
        (Fault::Allocate, 0),
        (Fault::Write, 1),
        (Fault::ShortWrite, 1),
        (Fault::Sync, 0),
        (Fault::Unlink, 0),
        (Fault::DirectorySync, 1),
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::create_sized(
            Tickets::open(root.path(), metadata()).unwrap(),
            config(),
            ChunkSize::K16,
            metadata(),
            metadata(),
        )
        .unwrap();
        let values = [[1; 16384], [2; 16384], [3; 16384]];
        let chunks: Vec<_> = values.iter().map(|v| Chunk::new(v).unwrap()).collect();
        store.insert(&chunks).unwrap();
        let mut collection = store.begin_collection().unwrap();
        collection.mark(&chunks[0].hash()).unwrap();
        collection.mark(&chunks[2].hash()).unwrap();
        let mut sweep = collection.finish_marking().unwrap();
        faults::inject_after(fault, after);
        assert!(sweep.clean_next().is_err());
        drop(sweep);
        drop(store);
        let store = open(root.path());
        let mut bytes = AlignedBuffer::new(16384);
        for i in [0, 2] {
            store
                .plan(chunks[i].hash())
                .unwrap()
                .unwrap()
                .load(bytes.as_mut_slice())
                .unwrap();
            assert!(bytes.as_slice() == values[i]);
        }
    }
}

#[test]
fn unsupported_or_mixed_recorded_geometry_is_rejected_without_repair() {
    use crate::encoding::{checksum, put32};
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::create_sized(
        Tickets::open(root.path(), metadata()).unwrap(),
        config(),
        ChunkSize::K16,
        metadata(),
        metadata(),
    )
    .unwrap();
    store.insert(&[Chunk::new(&[7; 16384]).unwrap()]).unwrap();
    drop(store);
    let path = root.path().join("chunk-format.v3");
    let original = fs::read(&path).unwrap();
    for (offset, value) in [(8, 2), (12, 8192), (12, 4096)] {
        let mut changed = original.clone();
        put32(&mut changed, offset, value);
        let crc = checksum(&changed, 4092);
        put32(&mut changed, 4092, crc);
        fs::write(&path, &changed).unwrap();
        assert!(
            Store::inspect_recorded(
                Tickets::open(root.path(), metadata()).unwrap(),
                metadata(),
                metadata(),
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), changed);
        assert!(!root.path().join("chunks/rejected").exists());
    }
    fs::remove_file(path).unwrap();
    assert!(
        Store::inspect_recorded(
            Tickets::open(root.path(), metadata()).unwrap(),
            metadata(),
            metadata(),
        )
        .is_err()
    );
}

#[test]
#[ignore = "requires an XFS reflink fixture"]
fn chunk16_snapshot_clone_collection_preserves_geometry_and_old_content() {
    use crate::manifest::file::Snapshot;
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::create_sized(
        Tickets::open(root.path(), metadata()).unwrap(),
        config(),
        ChunkSize::K16,
        metadata(),
        metadata(),
    )
    .unwrap();
    let values = [[1; 16384], [2; 16384], [3; 16384], [4; 16384]];
    let chunks: Vec<_> = values.iter().map(|v| Chunk::new(v).unwrap()).collect();
    store.insert(&chunks).unwrap();
    let identity = Identity {
        store: config().store,
        image: [2; 16],
        image_bytes: 16384,
    };
    let source_path = root.path().join("source");
    let snapshot_path = root.path().join("snapshot");
    let clone_path = root.path().join("clone");
    for path in [&source_path, &snapshot_path, &clone_path] {
        fs::create_dir(path).unwrap();
    }
    let mut source =
        Manifest::create_sized(&source_path, identity, ChunkSize::K16, metadata()).unwrap();
    let publish = |manifest: &mut Manifest, hash, durable| {
        let prepared = manifest
            .prepare(
                &[Extent {
                    start: 0,
                    end: 4,
                    hash: Some(hash),
                }],
                durable,
            )
            .unwrap();
        manifest.publish(prepared).unwrap();
    };
    publish(&mut source, chunks[0].hash(), 1);
    let mut snapshot =
        Snapshot::create(&source.view().unwrap(), &snapshot_path, metadata()).unwrap();
    let clone_identity = Identity {
        image: [3; 16],
        ..identity
    };
    let mut clone =
        Manifest::clone_snapshot(&snapshot, &clone_path, clone_identity, metadata()).unwrap();
    assert_eq!(clone.chunk_size(), ChunkSize::K16);
    assert_eq!(clone.current().durable, 0);
    publish(&mut source, chunks[1].hash(), 2);
    publish(&mut clone, chunks[2].hash(), 1);
    let mut collection = store.begin_collection().unwrap();
    for roots in [
        source.pinned_roots(metadata()).unwrap(),
        snapshot.pinned_roots(metadata()).unwrap(),
        clone.pinned_roots(metadata()).unwrap(),
    ] {
        roots
            .walk(|extent| {
                if let Some(hash) = extent.hash {
                    collection.mark(&hash)?;
                }
                Ok(())
            })
            .unwrap();
    }
    let mut sweep = collection.finish_marking().unwrap();
    while sweep.clean_next().unwrap().is_some() {}
    assert_eq!(sweep.finish().unwrap().chunks_copied, 3);
    let key = snapshot.key();
    drop((source, snapshot, clone, store));
    let store = open(root.path());
    let dependency = |hash| require(store.plan(hash)?.is_some(), "missing snapshot chunk");
    let source = Manifest::inspect(&source_path, identity, 2, metadata(), dependency)
        .unwrap()
        .recover()
        .unwrap();
    let snapshot = Snapshot::inspect(&snapshot_path, key, metadata(), dependency)
        .unwrap()
        .recover()
        .unwrap();
    let clone = Manifest::inspect(&clone_path, clone_identity, 1, metadata(), dependency)
        .unwrap()
        .recover()
        .unwrap();
    for (view, i) in [
        (source.view().unwrap(), 1),
        (snapshot.view().unwrap(), 0),
        (clone.view().unwrap(), 2),
    ] {
        assert_eq!(view.chunk_size(), ChunkSize::K16);
        for block in 0..4 {
            assert_eq!(
                view.tree().unwrap().get(block).unwrap(),
                Some(chunks[i].hash())
            );
        }
        let mut bytes = AlignedBuffer::new(16384);
        store
            .plan(chunks[i].hash())
            .unwrap()
            .unwrap()
            .load(bytes.as_mut_slice())
            .unwrap();
        assert_eq!(bytes.as_slice(), values[i]);
    }
    assert!(store.plan(chunks[3].hash()).unwrap().is_none());
}
