use super::*;

const LIVE_BYTES: usize = 256 * MAX_REQUEST_BYTES;
const IMAGE: Identity = Identity {
    image_bytes: 2 * LIVE_BYTES as u64,
    store: STORE.store,
    image: [2; 16],
};

fn populated(root: &Path, resources: &Resources) -> (Store, Vec<(Log, Manifest)>) {
    let tickets = Tickets::open(root, Arc::clone(&resources.metadata)).unwrap();
    let store_config = Config {
        segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
        ..STORE
    };
    let mut store = Store::create(
        Arc::clone(&tickets),
        store_config,
        Arc::clone(&resources.metadata),
        resources.read_memory(),
    )
    .unwrap();
    let source_path = root.join("seed");
    let snapshot_path = root.join("seed-snapshot");
    fs::create_dir(&source_path).unwrap();
    fs::create_dir(&snapshot_path).unwrap();
    fs::create_dir_all(path(root, 2)).unwrap();
    let mut source = Manifest::create(
        &source_path,
        Identity {
            image: [4; 16],
            ..IMAGE
        },
        Arc::clone(&resources.metadata),
    )
    .unwrap();
    let mut edits = Vec::with_capacity(LIVE_BYTES / BLOCK_SIZE);
    insert_unique_chunks(&mut store, LIVE_BYTES, |batch| {
        edits.extend_from_slice(batch)
    });
    for (index, batch) in edits
        .chunks(cas_core::manifest::tree::MAX_CHANGES)
        .enumerate()
    {
        let prepared = source
            .prepare_with_metadata(batch, index as u64 + 1, Arc::clone(&resources.compaction))
            .unwrap();
        source.publish(prepared).unwrap();
    }
    drop(edits);
    // Only the final seed root is needed when creating the snapshot.
    source
        .reclaim_pages(Arc::clone(&resources.compaction))
        .unwrap();
    let snapshot = Snapshot::create(
        &source.view().unwrap(),
        &snapshot_path,
        Arc::clone(&resources.metadata),
    )
    .unwrap();
    let mut manifest = Manifest::clone_snapshot(
        &snapshot,
        &path(root, 2),
        IMAGE,
        Arc::clone(&resources.metadata),
    )
    .unwrap();
    drop((snapshot, source));
    fs::remove_dir_all(snapshot_path).unwrap();
    fs::remove_dir_all(source_path).unwrap();
    manifest
        .reclaim_pages(Arc::clone(&resources.compaction))
        .unwrap();
    let log = Log::create_shared(
        tickets,
        append::Config {
            store: STORE.store,
            image: IMAGE.image,
            image_bytes: IMAGE.image_bytes,
            segment_bytes: store_config.segment_bytes,
        },
        append::Limits::default(),
        Arc::clone(&resources.metadata),
        manifest.view().unwrap(),
    )
    .unwrap();
    assert_eq!(store.status().chunks, LIVE_BYTES / BLOCK_SIZE);
    (store, vec![(log, manifest)])
}

#[test]
#[ignore = "requires an exclusive XFS physical-account and reflink fixture"]
fn unique_live_payload_reports_capacity_exhaustion_and_preserves_reads() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let started = Instant::now();
    let (store, images) = populated(root.path(), &resources);
    let seed_micros = started.elapsed().as_micros();
    let initial = cas_core::space::Observation::inspect(store.tickets()).unwrap();
    let reserve = cas_core::space::Limits::new(
        initial.capacity(),
        store.config().segment_bytes,
        cas_core::manifest::tree::MAX_TRANSACTION_BYTES as u64,
    )
    .unwrap()
    .reserve;
    let limits = cas_core::space::Limits {
        capacity: initial.allocated + reserve,
        reserve,
    };
    let physical = cas_core::space::Governor::open(Arc::clone(store.tickets()), limits).unwrap();
    let mut host = Host::governed(
        Arc::clone(&resources),
        store,
        images,
        Arc::clone(&physical),
        1024 * MAX_REQUEST_BYTES as u64,
    )
    .unwrap();
    let event = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut local = loop {
        match host.local(IMAGE.image, &event) {
            Ok(local) => break local,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
            Err(error) => panic!("attachment failed: {error}"),
        }
        assert!(Instant::now() < deadline, "collection stranded attachment");
        thread::sleep(Duration::from_millis(1));
    };
    while host.report()["collection"]["completed"] == 0 {
        assert!(
            Instant::now() < deadline,
            "collection did not finish: {}",
            host.report()
        );
        thread::sleep(Duration::from_millis(1));
    }
    eprintln!(
        "capacity conditions: {}",
        serde_json::json!({"live_bytes": LIVE_BYTES, "initial": initial,
            "limits": limits, "after_collection": physical.status(), "host": host.report()})
    );
    assert_eq!(
        host.report()["collection"]["last"]["capacity_exhausted"],
        true
    );
    assert!(local.prepare(Kind::Write(BLOCK_SIZE)).unwrap().is_none());
    assert_eq!(local.admitted, 0);
    let reading = Instant::now();
    let mut expected = vec![0; MAX_REQUEST_BYTES];
    for offset in (0..LIVE_BYTES).step_by(expected.len()) {
        for (index, block) in expected
            .as_chunks_mut::<BLOCK_SIZE>()
            .0
            .iter_mut()
            .enumerate()
        {
            block[..8].copy_from_slice(&((offset / BLOCK_SIZE + index + 1) as u64).to_le_bytes());
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let permit = loop {
            if let Some(permit) = local.prepare(Kind::Read(expected.len())).unwrap() {
                break permit;
            }
            assert!(Instant::now() < deadline, "full store stranded a read");
            thread::sleep(Duration::from_millis(1));
        };
        local
            .enqueue(
                offset as u64,
                Operation::Read {
                    offset: offset as u64,
                    buffer: AlignedBuffer::new(expected.len()),
                },
                permit,
            )
            .unwrap();
        let completion = completed(&mut local);
        let CompletionData::Read(bytes) = completion.data else {
            panic!("read response");
        };
        assert_eq!(bytes.as_slice(), expected);
    }
    let read_micros = reading.elapsed().as_micros();
    let after = physical.status();
    assert!(after.pressured && !after.failed);
    assert!(u128::from(after.allocated) * 100 >= u128::from(limits.capacity) * 60);
    assert!(after.allocated <= limits.capacity - reserve);
    assert_eq!(host.store_status().chunks, LIVE_BYTES / BLOCK_SIZE);
    assert!(local.prepare(Kind::Write(BLOCK_SIZE)).unwrap().is_none());
    assert_eq!(local.admitted, 0);
    if let Some(path) = std::env::var_os("CAS_SPACE_REPORT").map(std::path::PathBuf::from) {
        fs::write(
            path.with_file_name("unique-live-capacity.json"),
            serde_json::to_vec_pretty(
                &serde_json::json!({"live_bytes":LIVE_BYTES, "read_bytes":LIVE_BYTES,
                "seed_micros":seed_micros, "read_micros":read_micros,
                "initial":initial, "limits":limits, "after":after, "host":host.report(),
                "admitted_mutations":local.admitted}),
            )
            .unwrap(),
        )
        .unwrap();
    }
    drop(local);
    shutdown(host);
    drop(physical);
    assert_eq!(resources.metadata.usage().current, Amount::default());
    assert_eq!(resources.compaction.usage().current, Amount::default());
}
