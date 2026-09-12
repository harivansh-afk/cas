use super::*;
use cas_core::space::{Governor, Observation};

const SEGMENT: u64 = 2 * MAX_REQUEST_BYTES as u64;

fn limits() -> append::Limits {
    append::Limits {
        staging_bytes: 2 * SEGMENT,
        ..append::Limits::default()
    }
}

#[test]
fn startup_rejects_staging_caps_that_cannot_reach_the_resume_watermark() {
    for (image_cap, host_cap) in [(SEGMENT, 8 * SEGMENT), (2 * SEGMENT, 3 * SEGMENT)] {
        let root = tempfile::tempdir().unwrap();
        let resources = Arc::new(Resources::default());
        let (store, images) = create_images(
            root.path(),
            2,
            &resources,
            IMAGE_BYTES,
            append::Limits {
                staging_bytes: image_cap,
                ..append::Limits::default()
            },
            STORE,
        );
        assert!(
            Host::build(
                Arc::clone(&resources),
                store,
                Roots {
                    images,
                    snapshots: Vec::new()
                },
                host_cap,
                None
            )
            .is_err()
        );
        assert_eq!(resources.metadata.usage().current.bytes, 0);
    }
}

#[test]
fn staging_pressure_drains_a_continuous_writer_without_guest_flushes() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let (store, images) = create_images(
        root.path(),
        2,
        &resources,
        MAX_REQUEST_BYTES as u64,
        limits(),
        STORE,
    );
    let mut host = Host::build(
        resources,
        store,
        Roots {
            images,
            snapshots: Vec::new(),
        },
        4 * SEGMENT,
        None,
    )
    .unwrap();
    let mut local = attach(&mut host, 2);
    let mut model = vec![0; MAX_REQUEST_BYTES];
    for sequence in 0..8 {
        model.fill(sequence as u8 + 1);
        write(&mut local, sequence, 0, &model);
        // No FLUSH and no idle sleep: every next max-sized request needs a new
        // WAL window. Capacity must return through finite sync and compaction.
        let (host_usage, image) = host.shared.staging.status(0).unwrap();
        assert!(host_usage.allocated + host_usage.promised <= 4 * SEGMENT);
        assert!(image.allocated + image.promised <= 2 * SEGMENT);
    }
    assert!(local.report()["status"]["compacted"].as_u64().unwrap() >= 7);
    read(&mut local, 8, &model);
    assert!(host.shared.staging.admits(1));
    drop(local);
    shutdown(host);
}

#[test]
fn failed_shared_account_precedes_read_completion_on_every_image() {
    let root = tempfile::tempdir().unwrap();
    let mut host = create(root.path(), 2, Arc::new(Resources::default()));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    assert!(host.shared.staging.reclaimed(0, u64::MAX).is_err());
    for local in [&mut first, &mut second] {
        let permit = local.prepare(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
        let _ = local.enqueue(
            0,
            Operation::Read {
                offset: 0,
                buffer: AlignedBuffer::new(BLOCK_SIZE),
            },
            permit,
        );
        let completed = received(local);
        assert!(completed.result.is_err());
    }
    assert!(host.shared.gate.failure().is_some());
    drop((first, second));
    shutdown(host);
}

#[test]
#[ignore = "requires the packaged dedicated XFS fixture and CAS_SPACE_REPORT"]
fn physical_promises_defer_rotation_and_real_reclamation_reopens_staging() {
    let report = std::path::PathBuf::from(
        std::env::var_os("CAS_SPACE_REPORT").expect("run packaged XFS fixture"),
    )
    .with_file_name("host-space-observations.json");
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let (store, images) = create_images(
        root.path(),
        2,
        &resources,
        8 * MAX_REQUEST_BYTES as u64,
        limits(),
        Config {
            segment_bytes: SEGMENT,
            ..STORE
        },
    );
    let initial = Observation::inspect(store.tickets()).unwrap();
    let reserve = cas_core::space::Limits::new(
        initial.capacity(),
        SEGMENT,
        cas_core::manifest::tree::MAX_TRANSACTION_BYTES as u64,
    )
    .unwrap()
    .reserve;
    let capacity = initial.allocated + reserve + 64 * MAX_REQUEST_BYTES as u64;
    let physical = Governor::open(
        Arc::clone(store.tickets()),
        cas_core::space::Limits { capacity, reserve },
    )
    .unwrap();
    // A separate accepted allocation owns this promise while the worker tries
    // to acquire its successor. It writes nothing in this control.
    let held = physical.foreground(56 * MAX_REQUEST_BYTES as u64).unwrap();
    let mut host =
        Host::governed(resources, store, images, Arc::clone(&physical), 4 * SEGMENT).unwrap();
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    let old = vec![5; MAX_REQUEST_BYTES];
    let phase = |name: &str, host: &Host| {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(report.with_extension("jsonl"))
            .unwrap();
        serde_json::to_writer(
            &mut file,
            &serde_json::json!({ "phase": name, "host": host.report() }),
        )
        .unwrap();
        writeln!(file).unwrap();
    };
    let (entered, observed) = mpsc::channel();
    let (release_collection, resume) = mpsc::channel();
    host.shared.control.lock().unwrap().collection = Some(Pause { entered, resume });
    phase("started", &host);
    write(&mut first, 0, 0, &old);
    phase("first-write", &host);
    assert!(
        first
            .prepare(Kind::Write(MAX_REQUEST_BYTES))
            .unwrap()
            .is_none()
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while !physical.status().pressured {
        assert!(
            Instant::now() < deadline,
            "rotation did not reach physical admission"
        );
        thread::sleep(Duration::from_millis(1));
    }
    observed.recv_timeout(Duration::from_secs(3)).unwrap();
    phase("physical-denial", &host);
    let denied = host.report();
    assert_eq!(physical.status().promised, 56 * MAX_REQUEST_BYTES as u64);
    assert_eq!(first.admitted, 1);
    assert!(
        first
            .prepare(Kind::Write(MAX_REQUEST_BYTES))
            .unwrap()
            .is_none()
    );
    assert!(second.prepare(Kind::Write(BLOCK_SIZE)).unwrap().is_none());
    assert_eq!(host.report()["admission"]["running"], true);
    // Physical pressure now invokes host GC. Reads resume after that pause;
    // the original accepted promise remains charged until its real release.
    drop(held);
    phase("released", &host);
    release_collection.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while host.shared.admission.status().paused {
        assert!(
            Instant::now() < deadline,
            "collection did not resume admission"
        );
        thread::sleep(Duration::from_millis(1));
    }
    read_at(&mut first, 1, 0, &old);
    phase("old-read", &host);
    read_at(&mut second, 0, 0, &[0; BLOCK_SIZE]);
    let permit = first.prepare(Kind::Control).unwrap().unwrap();
    first.enqueue(2, Operation::Flush, permit).unwrap();
    completed(&mut first);
    phase("covered-flush", &host);
    assert_eq!(first.status.durable, 1);
    assert!(!host.report()["collection_required"].as_bool().unwrap());
    first
        .pause(Instant::now() + Duration::from_secs(3))
        .unwrap();
    phase("paused", &host);
    assert!(!first.status.rotating);
    first.resume().unwrap();
    let mut expected = vec![0; 8 * MAX_REQUEST_BYTES];
    expected[..MAX_REQUEST_BYTES].copy_from_slice(&old);
    let mut samples = Vec::new();
    for block in 1..8 {
        let mut data = vec![block as u8 + 10; MAX_REQUEST_BYTES];
        for (index, chunk) in data.as_chunks_mut::<BLOCK_SIZE>().0.iter_mut().enumerate() {
            chunk[..8].copy_from_slice(&(block as u64 * 256 + index as u64).to_le_bytes());
        }
        write(
            &mut first,
            block as u64 + 2,
            block * MAX_REQUEST_BYTES,
            &data,
        );
        phase("next-write", &host);
        expected[block * MAX_REQUEST_BYTES..(block + 1) * MAX_REQUEST_BYTES].copy_from_slice(&data);
        samples.push(host.report());
    }
    drained(&mut first, 8);
    phase("compacted", &host);
    for (index, bytes) in expected
        .as_chunks::<MAX_REQUEST_BYTES>()
        .0
        .iter()
        .enumerate()
    {
        read_at(
            &mut first,
            20 + index as u64,
            (index * MAX_REQUEST_BYTES) as u64,
            bytes,
        );
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while physical.status().promised != 0 || physical.status().background_active {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let final_status = host.report();
    assert!(!physical.status().failed && !host.shared.staging.failed());
    assert_eq!(host.shared.staging.status(0).unwrap().0.promised, 0);
    assert!(first.report()["status"]["segments"].as_u64().unwrap() <= 2);
    fs::write(
        &report,
        serde_json::to_vec_pretty(&serde_json::json!({
            "initial": initial, "limits": physical.limits(), "denied": denied,
            "samples": samples, "final": final_status,
            "mutations": 8, "oracle_bytes": expected.len(), "oracle_passed": true,
        }))
        .unwrap(),
    )
    .unwrap();
    drop((first, second));
    phase("detached", &host);
    shutdown(host);
}

#[test]
fn disconnect_cancels_capacity_denied_future_rollover() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let (store, images) = create_images(
        root.path(),
        2,
        &resources,
        MAX_REQUEST_BYTES as u64,
        limits(),
        STORE,
    );
    let mut host = Host::build(
        resources,
        store,
        Roots {
            images,
            snapshots: Vec::new(),
        },
        4 * SEGMENT,
        None,
    )
    .unwrap();
    let mut local = attach(&mut host, 2);
    write(&mut local, 0, 0, &vec![1; MAX_REQUEST_BYTES]);
    let held = cas_core::space::Staging::reserve(&host.shared.staging, 0, SEGMENT).unwrap();
    local.shared.window.as_ref().unwrap().close();
    notify(local.input_wake.as_ref().unwrap()).unwrap();
    assert!(!host.shared.staging.admits(0));
    let (done, observed) = mpsc::sync_channel(1);
    let dropping = thread::spawn(move || {
        drop(local);
        done.send(()).unwrap();
    });
    observed
        .recv_timeout(Duration::from_secs(3))
        .expect("future-only allocation kept frontend alive");
    dropping.join().unwrap();
    assert_eq!(host.shared.staging.status(0).unwrap().0.promised, SEGMENT);
    drop(held);
    shutdown(host);
}

#[test]
fn terminal_close_drains_commands_beyond_a_queued_pause() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 1, Arc::clone(&resources));
    let mut local = attach(&mut host, 2);
    let first = local.prepare(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    let control = local.prepare(Kind::Control).unwrap().unwrap();
    let second = local.prepare(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    let shared = local.shared.clone();
    // Stop the reactor at its completion gate until all accepted commands are
    // queued. Terminal drain must cross Pause after this shared failure.
    let publication = shared.health.lock().unwrap();
    assert!(host.shared.staging.reclaimed(0, u64::MAX).is_err());
    let read = |id, permit| {
        Command::Io(Io {
            id,
            permit,
            boundary: 0,
            operation: Operation::Read {
                offset: 0,
                buffer: AlignedBuffer::new(BLOCK_SIZE),
            },
        })
    };
    let sender = local.sender.as_ref().unwrap();
    let (done, pause) = mailbox::bounded(1, &shared.metadata).unwrap();
    assert!(sender.try_send(read(0, first)).is_ok());
    assert!(
        sender
            .try_send(Command::Pause {
                done,
                _permit: control
            })
            .is_ok()
    );
    assert!(sender.try_send(read(1, second)).is_ok());
    drop(publication);
    notify(local.input_wake.as_ref().unwrap()).unwrap();
    for id in 0..2 {
        let response = received(&mut local);
        assert_eq!(response.id, id);
        assert!(response.result.is_err());
    }
    assert!(pause.recv_timeout(Duration::from_secs(3)).unwrap().is_err());
    drop((pause, shared, local));
    shutdown(host);
    assert_eq!(resources.read_memory().usage().current.bytes, 0);
    assert_eq!(resources.metadata.usage().current.bytes, 0);
}

#[test]
fn surviving_shared_state_and_health_keep_their_allocation_charges() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 1, Arc::clone(&resources));
    let local = attach(&mut host, 2);
    let shared = local.shared.clone();
    let health = shared.health.clone();
    let host_state = host.shared.clone();
    drop(local);
    shutdown(host);
    let retained = resources.metadata.usage().current.bytes;
    assert!(retained > 0);
    drop(host_state);
    drop(shared);
    let gates = resources.metadata.usage().current.bytes;
    assert!(gates > 0 && gates < retained);
    health.lock().unwrap().publish(1).unwrap();
    drop(health);
    assert_eq!(resources.metadata.usage().current, Amount::default());
    assert_eq!(
        resources.metadata.usage().admitted,
        resources.metadata.usage().released
    );
}
