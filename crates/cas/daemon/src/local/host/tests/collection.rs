use super::*;

#[test]
fn collection_waits_for_guest_owners_and_caller_timeout_does_not_resume_running_io() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    write(&mut first, 0, 0, &[7; BLOCK_SIZE]);
    drained(&mut first, 1);
    let unused = first.prepare(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    let (entered, observed) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    host.shared.control.lock().unwrap().collection = Some(Pause { entered, resume });
    let handle = host.collect().unwrap();
    assert!(host.collect().is_err());
    assert_eq!(
        handle.wait(Duration::from_millis(30)).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(host.report()["admission"]["running"], false);
    assert!(matches!(
        observed.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(second.prepare(Kind::Read(BLOCK_SIZE)).unwrap().is_none());
    drop(unused);
    observed.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(host.report()["admission"]["running"], true);
    drop(handle);
    assert!(host.collect().is_err());
    assert!(first.prepare(Kind::Control).unwrap().is_none());
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while host.shared.admission.status().paused {
        assert!(
            Instant::now() < deadline,
            "collector did not resume: {}",
            host.report()
        );
        thread::sleep(Duration::from_millis(1));
    }
    read(&mut first, 1, &[7; BLOCK_SIZE]);
    read(&mut second, 0, &[0; BLOCK_SIZE]);
    drop((first, second));
    shutdown(host);
    assert_eq!(resources.metadata.usage().current, Amount::default());
    assert_eq!(resources.compaction.usage().current, Amount::default());
}

#[test]
fn collection_preserves_both_images_through_overwrite_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    let old = vec![3; IMAGE_BYTES as usize];
    let new = vec![9; IMAGE_BYTES as usize];
    write(&mut first, 0, 0, &old);
    write(&mut second, 0, 0, &old);
    drained(&mut first, 1);
    drained(&mut second, 1);
    write(&mut first, 1, 0, &new);
    drained(&mut first, 2);
    let handle = host.collect().unwrap();
    let report = handle.wait(Duration::from_secs(5)).unwrap();
    assert_eq!(report.rounds, 1);
    assert!(report.manifests.roots >= 2);
    assert!(!report.capacity_exhausted);
    drop(handle);
    read(&mut first, 2, &new);
    read(&mut second, 1, &old);
    drop((first, second));
    shutdown(host);
    assert_eq!(resources.metadata.usage().current, Amount::default());
    let mut host = reopen(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    read(&mut first, 0, &new);
    read(&mut second, 0, &old);
    drop((first, second));
    shutdown(host);
}

#[test]
fn later_attachment_barriers_wait_until_collection_releases_the_reactor() {
    let root = tempfile::tempdir().unwrap();
    let mut host = create(root.path(), 1, Arc::new(Resources::default()));
    let mut local = attach(&mut host, 2);
    write(&mut local, 0, 0, &[4; BLOCK_SIZE]);
    drained(&mut local, 1);
    let (entered, observed) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    host.shared.control.lock().unwrap().collection = Some(Pause { entered, resume });
    let handle = host.collect().unwrap();
    observed.recv_timeout(Duration::from_secs(3)).unwrap();
    thread::scope(|scope| {
        let (finished, completion) = mpsc::channel();
        let task = scope.spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            local.pause(deadline).unwrap();
            local.new_attachment(deadline).unwrap();
            local.resume().unwrap();
            finished.send(()).unwrap();
            local
        });
        assert!(matches!(
            completion.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        release.send(()).unwrap();
        handle.wait(Duration::from_secs(3)).unwrap();
        completion.recv_timeout(Duration::from_secs(3)).unwrap();
        let mut local = task.join().unwrap();
        read(&mut local, 1, &[4; BLOCK_SIZE]);
    });
    drop(handle);
    shutdown(host);
}

#[test]
fn failure_after_quiescence_begins_closes_the_host_before_any_later_admission() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 1, Arc::clone(&resources));
    let mut local = attach(&mut host, 2);
    write(&mut local, 0, 0, &[4; BLOCK_SIZE]);
    drained(&mut local, 1);
    let (entered, observed) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    host.shared.control.lock().unwrap().collection = Some(Pause { entered, resume });
    let handle = host.collect().unwrap();
    observed.recv_timeout(Duration::from_secs(3)).unwrap();
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
    assert!(local.shared.reserve(Kind::Read(BLOCK_SIZE)).is_none());
    assert!(local.shared.health.lock().unwrap().failure.is_some());
    drop((held, handle, local));
    shutdown(host);
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

#[test]
#[ignore = "requires an XFS reflink fixture"]
fn snapshot_and_private_clone_roots_survive_host_collection() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 1, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    write(&mut first, 0, 0, &[4; BLOCK_SIZE]);
    drained(&mut first, 1);
    drop(first);
    shutdown(host);
    let (store, mut images) = recovered_images(root.path(), 1, &resources);
    let captured = images[0].1.view().unwrap();
    let snapshot_path = root.path().join("snapshot");
    fs::create_dir(&snapshot_path).unwrap();
    let snapshot =
        Snapshot::create(&captured, &snapshot_path, Arc::clone(&resources.metadata)).unwrap();
    let snapshot_view = snapshot.view().unwrap();
    fs::create_dir(path(root.path(), 3)).unwrap();
    let clone = Manifest::clone_snapshot(
        &snapshot,
        &path(root.path(), 3),
        identity(3),
        Arc::clone(&resources.metadata),
    )
    .unwrap();
    let log = Log::create_shared(
        Arc::clone(store.tickets()),
        append::Config {
            image: [3; 16],
            ..images[0].0.config()
        },
        append::Limits::default(),
        Arc::clone(&resources.metadata),
        clone.view().unwrap(),
    )
    .unwrap();
    images.push((log, clone));
    let initial = cas_core::space::Observation::inspect(store.tickets()).unwrap();
    let physical = cas_core::space::Governor::open(
        Arc::clone(store.tickets()),
        cas_core::space::Limits::new(
            initial.capacity(),
            store.config().segment_bytes,
            cas_core::manifest::tree::MAX_TRANSACTION_BYTES as u64,
        )
        .unwrap(),
    )
    .unwrap();
    let mut host = Host::from_roots(
        Arc::clone(&resources),
        store,
        Roots {
            images,
            snapshots: vec![snapshot],
        },
        1024 * MAX_REQUEST_BYTES as u64,
        Some(physical),
    )
    .unwrap();
    let mut first = attach(&mut host, 2);
    let mut clone = attach(&mut host, 3);
    write(&mut first, 0, 0, &[7; BLOCK_SIZE]);
    write(&mut clone, 0, 0, &[9; BLOCK_SIZE]);
    drained(&mut first, 2);
    drained(&mut clone, 1);
    let handle = host.collect().unwrap();
    let report = handle.wait(Duration::from_secs(5)).unwrap();
    assert!(report.manifests.roots >= 4); // Active, clone, snapshot and old View.
    drop(handle);
    read(&mut first, 1, &[7; BLOCK_SIZE]);
    read(&mut clone, 1, &[9; BLOCK_SIZE]);
    for view in [&snapshot_view, &captured] {
        let hash = view.tree().unwrap().get(0).unwrap().unwrap();
        let mut bytes = AlignedBuffer::new(BLOCK_SIZE);
        host.shared
            .reader
            .plan(hash)
            .unwrap()
            .unwrap()
            .load(bytes.as_mut_slice())
            .unwrap();
        assert_eq!(bytes.as_slice(), &[4; BLOCK_SIZE]);
    }
    drop((first, clone));
    shutdown(host);
    drop((captured, snapshot_view));
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

#[test]
#[ignore = "requires an exclusive XFS physical-account fixture"]
fn automatic_collection_restores_write_admission_after_physical_pressure() {
    use cas_core::chunk::Chunk;
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let config = Config {
        segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
        ..STORE
    };
    let (mut store, images) = create_images(
        root.path(),
        1,
        &resources,
        IMAGE_BYTES,
        append::Limits::default(),
        config,
    );
    // Preexisting unreachable contents represent a recovered store. Freeze the
    // payload volume and admission headroom before measuring the collector.
    const GARBAGE_BYTES: usize = 96 * MAX_REQUEST_BYTES;
    let mut blocks = [[0u8; BLOCK_SIZE]; cas_core::store::format::MAX_CHUNKS];
    for first in (0..GARBAGE_BYTES / BLOCK_SIZE).step_by(blocks.len()) {
        let count = blocks.len().min(GARBAGE_BYTES / BLOCK_SIZE - first);
        for (offset, block) in blocks[..count].iter_mut().enumerate() {
            block[..8].copy_from_slice(&((first + offset + 1) as u64).to_le_bytes());
        }
        let chunks: Vec<_> = blocks[..count]
            .iter()
            .map(|block| Chunk::new(block).unwrap())
            .collect();
        store.insert(&chunks).unwrap();
    }
    let before = cas_core::space::Observation::inspect(store.tickets()).unwrap();
    let reserve = cas_core::space::Limits::new(
        before.capacity(),
        config.segment_bytes,
        cas_core::manifest::tree::MAX_TRANSACTION_BYTES as u64,
    )
    .unwrap()
    .reserve;
    let limits = cas_core::space::Limits {
        capacity: before.allocated + reserve + 8 * MAX_REQUEST_BYTES as u64,
        reserve,
    };
    let physical = cas_core::space::Governor::open(Arc::clone(store.tickets()), limits).unwrap();
    let conditions = serde_json::json!({"payload_bytes":GARBAGE_BYTES, "initial":before,
        "limits":limits, "headroom_bytes":8 * MAX_REQUEST_BYTES,
        "denied_promise_bytes":16 * MAX_REQUEST_BYTES });
    let report_path = std::env::var_os("CAS_SPACE_REPORT")
        .map(std::path::PathBuf::from)
        .map(|path| path.with_file_name("host-collection-observations.json"));
    if let Some(path) = &report_path {
        fs::write(
            path.with_file_name("host-collection-conditions.json"),
            serde_json::to_vec_pretty(&conditions).unwrap(),
        )
        .unwrap();
    }
    let mut host = Host::governed(
        Arc::clone(&resources),
        store,
        images,
        Arc::clone(&physical),
        1024 * MAX_REQUEST_BYTES as u64,
    )
    .unwrap();
    let mut local = attach(&mut host, 2);
    let unused = local.prepare(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    assert_eq!(
        physical
            .foreground(16 * MAX_REQUEST_BYTES as u64)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(local.prepare(Kind::Write(BLOCK_SIZE)).unwrap().is_none());
    assert_eq!(local.admitted, 0);
    let deadline = Instant::now() + Duration::from_secs(3);
    while !host.shared.admission.status().paused {
        assert!(
            Instant::now() < deadline,
            "automatic pressure collection did not start"
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert!(!host.shared.admission.status().running);
    drop(unused);
    write(&mut local, 0, 0, &[5; BLOCK_SIZE]);
    drained(&mut local, 1);
    read(&mut local, 1, &[5; BLOCK_SIZE]);
    let deadline = Instant::now() + Duration::from_secs(3);
    let after = loop {
        let status = physical.status();
        assert!(!status.failed);
        if !status.background_active && status.promised == 0 {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "compaction observation still owned: {}",
            host.report()
        );
        thread::sleep(Duration::from_millis(1));
    };
    assert!(!after.pressured && !after.failed);
    assert_eq!(after.promised, 0);
    assert!(after.allocated * 100 < (limits.capacity - limits.reserve) * 60);
    assert!(before.allocated - after.allocated >= GARBAGE_BYTES as u64);
    let report = host.report();
    assert!(report["collection"]["completed"].as_u64().unwrap() >= 1);
    assert_eq!(report["collection"]["last"]["capacity_exhausted"], false);
    assert!(
        report["collection"]["last"]["chunks"]["segments_removed"]
            .as_u64()
            .unwrap()
            > 0
    );
    if let Some(path) = report_path {
        fs::write(
            path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "conditions":conditions, "after":after, "host":report,
                "admitted_after":local.admitted, "read_oracle":true,
            }))
            .unwrap(),
        )
        .unwrap();
    }
    drop(local);
    shutdown(host);
    drop(physical);
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

#[test]
fn uncertain_drain_acknowledgment_fails_closed_before_reopening_admission() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let first = attach(&mut host, 2);
    let second = attach(&mut host, 3);
    host.shared.control.lock().unwrap().quiescence_error = true;
    let handle = host.collect().unwrap();
    assert!(handle.wait(Duration::from_secs(3)).is_err());
    assert!(host.shared.gate.failure().is_some());
    assert!(host.shared.admission.status().failed);
    assert!(host.shared.admission.status().paused);
    for local in [&first, &second] {
        assert!(local.shared.reserve(Kind::Read(BLOCK_SIZE)).is_none());
        assert!(local.shared.health.lock().unwrap().failure.is_some());
    }
    drop((handle, first, second));
    shutdown(host);
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

#[test]
#[ignore = "requires an exclusive XFS physical-account fixture"]
fn a_sweep_with_no_compaction_input_does_not_strand_later_background_work() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let config = Config {
        segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
        ..STORE
    };
    let (store, images) = create_images(
        root.path(),
        1,
        &resources,
        IMAGE_BYTES,
        append::Limits::default(),
        config,
    );
    let initial = cas_core::space::Observation::inspect(store.tickets()).unwrap();
    let reserve = cas_core::space::Limits::new(
        initial.capacity(),
        config.segment_bytes,
        cas_core::manifest::tree::MAX_TRANSACTION_BYTES as u64,
    )
    .unwrap()
    .reserve;
    let headroom = initial.allocated + 256 * MAX_REQUEST_BYTES as u64;
    let physical = cas_core::space::Governor::open(
        Arc::clone(store.tickets()),
        cas_core::space::Limits {
            capacity: initial.allocated + headroom + reserve,
            reserve,
        },
    )
    .unwrap();
    let mut host = Host::governed(
        Arc::clone(&resources),
        store,
        images,
        Arc::clone(&physical),
        1024 * MAX_REQUEST_BYTES as u64,
    )
    .unwrap();
    let mut local = attach(&mut host, 2);
    let held = physical
        .foreground(headroom - 8 * MAX_REQUEST_BYTES as u64)
        .unwrap();
    assert_eq!(
        physical
            .foreground(16 * MAX_REQUEST_BYTES as u64)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while host.report()["collection"]["completed"] == 0 {
        assert!(
            Instant::now() < deadline,
            "no-input sweep did not complete: {}",
            host.report()
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        host.report()["collection"]["last"]["capacity_exhausted"],
        true
    );
    assert_eq!(host.report()["collection"]["last"]["compactions"], 0);
    drop(held);
    physical.refresh().unwrap();
    assert!(!physical.status().pressured);
    write(&mut local, 0, 0, &[6; BLOCK_SIZE]);
    drained(&mut local, 1);
    read(&mut local, 1, &[6; BLOCK_SIZE]);
    drop(local);
    shutdown(host);
    drop(physical);
    assert_eq!(resources.metadata.usage().current, Amount::default());
}
