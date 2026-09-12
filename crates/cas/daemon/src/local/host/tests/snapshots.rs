use super::*;
use cas_core::catalog::Kind as EntryKind;

fn cold(root: &Path, resources: &Arc<Resources>) -> Host {
    let inspected = recovery::scan(root, resources).unwrap();
    let limits = recovery::physical_limits(&inspected);
    inspected
        .require(crate::recovery::Prefixes::Cold)
        .unwrap()
        .recover_cold(limits)
        .unwrap()
        .into_host(1024 * MAX_REQUEST_BYTES as u64)
        .unwrap()
}

#[test]
fn snapshot_without_catalog_rejects_without_pausing_or_failing_guests() {
    let root = tempfile::tempdir().unwrap();
    let mut host = create(root.path(), 1, Arc::new(Resources::default()));
    let mut image = attach(&mut host, 2);
    let handle = host.snapshot([2; 16], [9; 16]).unwrap();
    assert!(handle.wait(Duration::from_secs(3)).is_err());
    assert!(!host.shared.admission.status().paused);
    assert!(host.shared.gate.failure().is_none());
    read(&mut image, 0, &[0; BLOCK_SIZE]);
    drop((handle, image));
    shutdown(host);
}

#[test]
#[ignore = "requires isolated XFS with reflink and physical admission"]
fn shared_snapshot_fences_its_cut_and_preserves_old_data_through_overwrite_collection_and_restart()
{
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    recovery::setup(root.path(), &resources, recovery::SnapshotFixture::Absent);
    let mut host = cold(root.path(), &resources);
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    let old = [7; BLOCK_SIZE];
    write(&mut first, 0, 0, &old);
    write(&mut second, 0, 0, &old);
    // There is no guest FLUSH. Snapshot quiescence must fence accepted writes.
    let snapshot = host.snapshot([2; 16], [9; 16]).unwrap();
    let report = snapshot.wait(Duration::from_secs(5)).unwrap();
    assert_eq!(report.cut, 1);
    assert_eq!(report.snapshot, [9; 16]);
    assert_eq!(report.image, [2; 16]);
    assert!(report.manifest_generation > 1);
    drop(snapshot);
    for (image, id) in [([2; 16], [9; 16]), ([2; 16], [0; 16]), ([99; 16], [8; 16])] {
        assert!(
            host.snapshot(image, id)
                .unwrap()
                .wait(Duration::from_secs(3))
                .is_err()
        );
        assert!(host.shared.gate.failure().is_none());
        assert!(!host.shared.admission.status().paused);
    }
    write(&mut first, 1, 0, &[8; BLOCK_SIZE]);
    write(&mut second, 1, 0, &[9; BLOCK_SIZE]);
    drained(&mut first, 2);
    drained(&mut second, 2);
    host.collect()
        .unwrap()
        .wait(Duration::from_secs(5))
        .unwrap();
    read(&mut first, 2, &[8; BLOCK_SIZE]);
    read(&mut second, 2, &[9; BLOCK_SIZE]);
    drop((first, second));
    shutdown(host);
    assert_eq!(resources.metadata.usage().current, Amount::default());
    let inspected = recovery::scan(root.path(), &resources).unwrap();
    let EntryKind::Snapshot(key) = inspected.contents().get([9; 16]).unwrap().kind else {
        panic!("snapshot missing")
    };
    assert_eq!(key.commit.durable, report.cut);
    assert_eq!(key.end, report.manifest_end);
    drop(inspected);
    // An independent exact-key inspection verifies all referenced chunk bytes.
    let tickets = Tickets::open(root.path(), Arc::clone(&resources.metadata)).unwrap();
    let store = Store::inspect(
        Arc::clone(&tickets),
        Config {
            segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
            ..STORE
        },
        Arc::clone(&resources.metadata),
        resources.read_memory(),
    )
    .unwrap();
    let snapshot = Snapshot::inspect(
        &root.path().join("snapshots").join("09".repeat(16)),
        key,
        Arc::clone(&resources.metadata),
        |hash| {
            if store.contains(&hash) {
                Ok(())
            } else {
                Err(io::Error::other("missing snapshot chunk"))
            }
        },
    )
    .unwrap()
    .recover()
    .unwrap();
    let hash = cas_core::chunk::Chunk::new(&old).unwrap().hash();
    let mut extents = Vec::new();
    snapshot
        .view()
        .unwrap()
        .tree()
        .unwrap()
        .walk(|extent| {
            extents.push(extent);
            Ok(())
        })
        .unwrap();
    assert_eq!(extents.len(), 1);
    assert_eq!(extents[0].hash, Some(hash));
    let store = store.recover().unwrap();
    let mut payload = AlignedBuffer::new(BLOCK_SIZE);
    store
        .plan(hash)
        .unwrap()
        .unwrap()
        .load(payload.as_mut_slice())
        .unwrap();
    assert_eq!(payload.as_slice(), old);
    drop((snapshot, store, tickets));
    let mut host = cold(root.path(), &resources);
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    read(&mut first, 0, &[8; BLOCK_SIZE]);
    read(&mut second, 0, &[9; BLOCK_SIZE]);
    drop((first, second));
    shutdown(host);
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

#[test]
#[ignore = "requires isolated XFS with reflink and physical admission"]
fn abandoned_snapshot_handle_retains_pause_and_administrative_credit_until_publication() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    recovery::setup(root.path(), &resources, recovery::SnapshotFixture::Absent);
    let mut host = cold(root.path(), &resources);
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    write(&mut first, 0, 0, &[3; BLOCK_SIZE]);
    let (entered, observed) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    host.shared.control.lock().unwrap().snapshot = Some(Pause { entered, resume });
    let handle = host.snapshot([2; 16], [9; 16]).unwrap();
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        handle.wait(Duration::from_millis(30)).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    drop(handle);
    assert!(host.collect().is_err());
    assert!(host.snapshot([2; 16], [8; 16]).is_err());
    assert!(first.prepare(Kind::Control).unwrap().is_none());
    assert!(second.prepare(Kind::Read(BLOCK_SIZE)).unwrap().is_none());
    assert!(Tickets::open(root.path(), Arc::clone(&resources.metadata)).is_err());
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.shared.admission.status().paused {
        assert!(
            Instant::now() < deadline,
            "snapshot did not resume: {}",
            host.report()
        );
        thread::sleep(Duration::from_millis(1));
    }
    read(&mut first, 1, &[3; BLOCK_SIZE]);
    read(&mut second, 0, &[0; BLOCK_SIZE]);
    drop((first, second));
    shutdown(host);
    let inspected = recovery::scan(root.path(), &resources).unwrap();
    assert!(matches!(
        inspected.contents().get([9; 16]).unwrap().kind,
        EntryKind::Snapshot(_)
    ));
    drop(inspected);
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

#[test]
#[ignore = "requires isolated XFS with reflink and physical admission"]
fn snapshot_output_failure_closes_all_guests_and_retains_partial_directory() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    recovery::setup(root.path(), &resources, recovery::SnapshotFixture::Absent);
    let mut host = cold(root.path(), &resources);
    let first = attach(&mut host, 2);
    let second = attach(&mut host, 3);
    let (entered, observed) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    host.shared.control.lock().unwrap().snapshot = Some(Pause { entered, resume });
    let handle = host.snapshot([2; 16], [9; 16]).unwrap();
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    let held = resources
        .metadata
        .reserve(Amount {
            bytes: 128 * MAX_REQUEST_BYTES - resources.metadata.usage().current.bytes,
            requests: 0,
        })
        .unwrap();
    release.send(()).unwrap();
    assert_eq!(
        handle.wait(Duration::from_secs(3)).unwrap_err().kind(),
        io::ErrorKind::OutOfMemory
    );
    assert!(host.shared.gate.failure().is_some());
    assert!(host.shared.admission.status().failed);
    assert!(first.shared.reserve(Kind::Read(BLOCK_SIZE)).is_none());
    assert!(second.shared.reserve(Kind::Control).is_none());
    assert!(first.shared.health.lock().unwrap().failure.is_some());
    assert!(second.shared.health.lock().unwrap().failure.is_some());
    drop((held, handle, first, second));
    shutdown(host);
    let directory = root.path().join("snapshots").join("09".repeat(16));
    assert!(directory.is_dir());
    assert!(fs::read_dir(&directory).unwrap().next().is_none());
    let inspected = recovery::scan(root.path(), &resources).unwrap();
    assert!(inspected.contents().get([9; 16]).is_none());
    drop(inspected);
    assert_eq!(resources.metadata.usage().current, Amount::default());
}
