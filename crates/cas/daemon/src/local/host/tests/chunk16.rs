use super::*;
use cas_core::chunk::ChunkSize;

#[test]
fn chunk16_reactor_cache_partial_overwrite_collection_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources {
        cache_bytes: 32768,
        ..Resources::default()
    });
    let config = Config {
        segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
        ..STORE
    };
    let (store, images) = create_chunked_images(
        root.path(),
        2,
        &resources,
        IMAGE_BYTES,
        append::Limits::default(),
        config,
        ChunkSize::K16,
    );
    let mut host = start(Arc::clone(&resources), store, images);
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    let mut bytes = [0; 16384];
    for (i, block) in bytes.chunks_mut(BLOCK_SIZE).enumerate() {
        block.fill(i as u8 + 1);
    }
    for local in [&mut first, &mut second] {
        write(local, 0, 0, &bytes);
        drained(local, 1);
    }
    assert_eq!(host.store_status().chunks, 1);
    for (i, block) in bytes.chunks(BLOCK_SIZE).enumerate() {
        read_at(&mut first, i as u64 + 1, (i * BLOCK_SIZE) as u64, block);
    }
    read(&mut second, 1, &bytes);
    assert!(host.shared.cache.status().counters.hits >= 7);
    write(&mut first, 5, BLOCK_SIZE, &[8; BLOCK_SIZE]);
    drained(&mut first, 2);
    let mut changed = bytes;
    changed[BLOCK_SIZE..2 * BLOCK_SIZE].fill(8);
    let collection = host.collect().unwrap();
    collection.wait(Duration::from_secs(5)).unwrap();
    drop(collection);
    read(&mut first, 6, &changed);
    read(&mut second, 2, &bytes);
    let report = host.report();
    assert_eq!(report["store"]["chunk_bytes"], 16384);
    assert!(
        report["store"]["index_memory"]["allocated_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(report["cache"]["allocated_bytes"].as_u64().unwrap() >= 16384);
    drop((first, second));
    shutdown(host);
    let mut host = reopen(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    read(&mut first, 0, &changed);
    read(&mut second, 0, &bytes);
    drop((first, second));
    shutdown(host);
    assert_eq!(resources.metadata.usage().current.bytes, 0);
    assert_eq!(resources.read_memory().usage().current.bytes, 0);
}

#[test]
fn chunk16_rejects_peer_wire_configuration() {
    let resources = Resources {
        peer: crate::peer::Config {
            peer_listen: Some("127.0.0.1:0".parse().unwrap()),
            ..Default::default()
        },
        ..Resources::default()
    };
    assert!(resources.validate_chunk_size(16384).is_err());
    assert!(resources.validate_chunk_size(4096).is_ok());
}

#[test]
fn wrong_geometry_snapshot_rejects_the_catalog_before_any_repair() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    super::recovery::setup(
        root.path(),
        &resources,
        super::recovery::SnapshotFixture::Missing,
    );
    let directory = root.path().join("snapshots").join("09".repeat(16));
    fs::create_dir_all(&directory).unwrap();
    // The empty root has the catalog's exact COMMIT/end, but wrong FILE geometry.
    drop(
        Manifest::create_sized(
            &directory,
            identity(2),
            ChunkSize::K16,
            Arc::clone(&resources.metadata),
        )
        .unwrap(),
    );
    let before = super::recovery::files(root.path());
    let error = super::recovery::scan(root.path(), &resources)
        .err()
        .unwrap();
    assert!(error.to_string().contains("chunk geometry differs"));
    assert_eq!(super::recovery::files(root.path()), before);
}
