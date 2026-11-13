use super::*;
use crate::host_service::{self, Config as ServiceConfig, Endpoint, Mode};
use std::os::fd::AsRawFd;
use vhost::{
    VhostBackend,
    vhost_user::{VhostUserFrontend, message::VhostUserInflight},
};

fn config(root: &Path, transport: &Path, mode: Mode) -> ServiceConfig {
    ServiceConfig {
        peer: crate::peer::Config::default(),
        cache_bytes: Resources::DEFAULT_CACHE_BYTES,
        metadata_bytes: Resources::DEFAULT_METADATA_BYTES,
        telemetry: false,
        telemetry_rotate: false,
        root: root.to_owned(),
        store: STORE.store,
        segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
        staging_bytes: 1024 * MAX_REQUEST_BYTES as u64,
        mode,
        pause: None,
        attach_timeout: crate::deadline::RECOVERY_TIMEOUT,
        endpoints: (2..4)
            .map(|image| Endpoint {
                image: [image; 16],
                socket: transport.join(format!("{image}.sock")),
            })
            .collect(),
        reports: transport.to_owned(),
    }
}
fn inflight() -> VhostUserInflight {
    VhostUserInflight {
        mmap_size: 0,
        mmap_offset: 0,
        num_queues: 4,
        queue_size: 256,
    }
}
fn report(transport: &Path, name: &str) -> serde_json::Value {
    serde_json::from_reader(fs::File::open(transport.join(name)).unwrap()).unwrap()
}

#[test]
fn configured_membership_mismatch_reports_startup_failure_before_repair() {
    let root = tempfile::tempdir().unwrap();
    let transport = tempfile::tempdir_in("/dev/shm").unwrap();
    let resources = Arc::new(Resources::default());
    setup(root.path(), &resources, SnapshotFixture::Absent);
    let before = files(root.path());
    let mut config = config(root.path(), transport.path(), Mode::Cold);
    config.endpoints.pop();
    let error = host_service::serve(config).unwrap_err();
    assert!(error.to_string().contains("complete catalog image set"));
    assert_eq!(files(root.path()), before);
    assert_eq!(
        report(transport.path(), "host.json")["startup_error"],
        error.to_string()
    );
}

#[test]
fn failed_retained_negotiation_cancels_an_unconnected_peer_without_repair() {
    let root = tempfile::tempdir().unwrap();
    let transport = tempfile::tempdir_in("/dev/shm").unwrap();
    let resources = Arc::new(Resources::default());
    setup(root.path(), &resources, SnapshotFixture::Absent);
    let before = files(root.path());
    let config = config(root.path(), transport.path(), Mode::Retained);
    let (done, result) = mpsc::channel();
    let server = thread::spawn(move || done.send(host_service::serve(config)).unwrap());
    let mut frontend = super::frontend::connect(&transport.path().join("2.sock"));
    assert!(frontend.get_inflight_fd(&inflight()).is_err());
    assert!(
        result
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_err()
    );
    server.join().unwrap();
    assert_eq!(files(root.path()), before);
    for image in 2..4 {
        assert!(!transport.path().join(format!("{image}.sock")).exists());
        assert_eq!(
            report(
                transport.path(),
                &format!("{}.json", format!("{image:02x}").repeat(16))
            )["connection_ok"],
            false
        );
    }
    assert_eq!(report(transport.path(), "host.json")["services_ok"], false);
}

#[test]
#[ignore = "requires the exclusive XFS allocation fixture"]
fn cold_host_supervises_both_socket_services_and_drains_the_shared_owner() {
    let root = tempfile::tempdir().unwrap();
    let transport = tempfile::tempdir_in("/dev/shm").unwrap();
    let resources = Arc::new(Resources::default());
    setup(root.path(), &resources, SnapshotFixture::Present);
    let mut config = config(root.path(), transport.path(), Mode::Cold);
    config.telemetry = true;
    config.cache_bytes = 4 * MAX_REQUEST_BYTES;
    let (done, result) = mpsc::channel();
    let server = thread::spawn(move || done.send(host_service::serve(config)).unwrap());
    let mut frontends = Vec::new();
    for image in 2..4 {
        let mut frontend =
            super::frontend::connect(&transport.path().join(format!("{image}.sock")));
        let (message, file) = frontend.get_inflight_fd(&inflight()).unwrap();
        frontend
            .set_inflight_fd(&message, file.as_raw_fd())
            .unwrap();
        frontend.get_features().unwrap();
        frontends.push((frontend, file));
    }
    drop(frontends);
    result
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    server.join().unwrap();
    for image in 2..4 {
        assert_eq!(
            report(
                transport.path(),
                &format!("{}.json", format!("{image:02x}").repeat(16))
            )["connection_ok"],
            true
        );
    }
    let report = report(transport.path(), "host.json");
    assert_eq!(report["services_ok"], true);
    assert!(report["shutdown_error"].is_null());
    assert_eq!(report["metadata"]["current"]["bytes"], 0);
    let samples = fs::read_to_string(transport.path().join("telemetry.jsonl")).unwrap();
    let first: serde_json::Value = serde_json::from_str(samples.lines().next().unwrap()).unwrap();
    assert_eq!(first["phase"], "live");
    assert_eq!(first["images"].as_array().unwrap().len(), 2);
    assert_eq!(first["images"][0]["image"], "02".repeat(16));
    assert!(first["images"][0]["report"].get("connection_ok").is_none());
    assert!(first["images"][0]["report"].get("pending").is_some());
}

#[test]
#[ignore = "requires the exclusive XFS allocation fixture"]
fn failed_cold_image_protocol_leaves_the_other_socket_usable() {
    let root = tempfile::tempdir().unwrap();
    let transport = tempfile::tempdir_in("/dev/shm").unwrap();
    let resources = Arc::new(Resources::default());
    setup(root.path(), &resources, SnapshotFixture::Absent);
    let config = config(root.path(), transport.path(), Mode::Cold);
    let (done, result) = mpsc::channel();
    let server = thread::spawn(move || done.send(host_service::serve(config)).unwrap());
    // GET_INFLIGHT_FD normalizes geometry; use an invalid wire header instead.
    let mut bad = super::frontend::connect_stream(&transport.path().join("2.sock"));
    let mut good = super::frontend::connect(&transport.path().join("3.sock"));
    good.get_features().unwrap();
    std::io::Write::write_all(&mut bad, &[0xff; 12]).unwrap();
    // Wait for the failed service to finish reporting, not merely disconnect.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let path = transport.path().join(format!("{}.json", "02".repeat(16)));
        if fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .is_some_and(|r| r["connection_ok"] == false)
        {
            break;
        }
        assert!(Instant::now() < deadline, "failed image did not finish");
        thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(
        result.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    good.get_features().unwrap();
    let (message, file) = good.get_inflight_fd(&inflight()).unwrap();
    good.set_inflight_fd(&message, file.as_raw_fd()).unwrap();
    drop((good, bad, file));
    assert!(
        result
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_err()
    );
    server.join().unwrap();
    assert_eq!(
        report(transport.path(), &format!("{}.json", "03".repeat(16)))["connection_ok"],
        true
    );
    assert!(report(transport.path(), "host.json")["host"]["failure"].is_null());
}

/// A catalog root whose rebuilt chunk index is the dominant metadata cost:
/// 4,096 distinct chunks need a 8,192-bucket table (about 330 KiB), and the
/// allocator charges the old table while the replacement grows.
const INDEX_CHUNKS: usize = 4096;
const TIGHT_METADATA_BYTES: usize = 256 * 1024;
fn chunk_pattern(chunk: usize) -> [u8; BLOCK_SIZE] {
    let mut bytes = [0x5a; BLOCK_SIZE];
    bytes[..8].copy_from_slice(&(chunk as u64).to_le_bytes());
    bytes
}
fn large_index_root(root: &Path, resources: &Arc<Resources>) {
    let image_bytes = (INDEX_CHUNKS * BLOCK_SIZE) as u64;
    let (store, images) = create_images(
        root,
        1,
        resources,
        image_bytes,
        append::Limits::default(),
        Config {
            segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
            ..STORE
        },
    );
    let mut catalog = Catalog::create(
        Arc::clone(store.tickets()),
        STORE.store,
        Arc::clone(&resources.metadata),
    )
    .unwrap();
    let prepared = catalog
        .prepare(Change::Insert(Entry {
            id: [2; 16],
            kind: EntryKind::Image { image_bytes },
        }))
        .unwrap();
    catalog.publish(prepared).unwrap();
    drop(catalog);
    let mut host = start(Arc::clone(resources), store, images);
    let mut local = attach(&mut host, 2);
    let requests = INDEX_CHUNKS * BLOCK_SIZE / MAX_REQUEST_BYTES;
    let mut bytes = vec![0; MAX_REQUEST_BYTES];
    for request in 0..requests {
        for (block, chunk) in bytes.as_chunks_mut::<BLOCK_SIZE>().0.iter_mut().enumerate() {
            *chunk = chunk_pattern(request * (MAX_REQUEST_BYTES / BLOCK_SIZE) + block);
        }
        write(
            &mut local,
            request as u64 + 1,
            request * MAX_REQUEST_BYTES,
            &bytes,
        );
    }
    let permit = local.prepare(Kind::Control).unwrap().unwrap();
    local
        .enqueue(requests as u64 + 1, Operation::Flush, permit)
        .unwrap();
    completed(&mut local);
    drained(&mut local, requests as u64);
    assert_eq!(host.store_status().chunks, INDEX_CHUNKS);
    drop(local);
    shutdown(host);
}

#[test]
fn metadata_budget_decides_whether_a_large_rebuilt_index_can_be_served() {
    let root = tempfile::tempdir().unwrap();
    let transport = tempfile::tempdir_in("/dev/shm").unwrap();
    large_index_root(root.path(), &Arc::new(Resources::default()));
    let before = files(root.path());
    // Below the index's need, host startup fails with the existing explicit
    // budget error before any repair; `--metadata-bytes` sets this value.
    let mut config = config(root.path(), transport.path(), Mode::Cold);
    config.endpoints.truncate(1);
    config.metadata_bytes = TIGHT_METADATA_BYTES;
    let error = host_service::serve(config).unwrap_err();
    assert!(
        error.to_string().contains("metadata budget exhausted"),
        "{error}"
    );
    assert_eq!(files(root.path()), before);
    assert_eq!(
        report(transport.path(), "host.json")["startup_error"],
        error.to_string()
    );
    // The same root opens and serves every chunk under a sufficient budget.
    let resources = Arc::new(Resources {
        metadata: crate::local::metadata_budget_sized(Resources::DEFAULT_METADATA_BYTES),
        ..Resources::default()
    });
    let mut host = scan(root.path(), &resources)
        .unwrap()
        .require(Prefixes::Cold)
        .unwrap()
        .recover_test(1024 * MAX_REQUEST_BYTES as u64)
        .unwrap();
    assert_eq!(host.store_status().chunks, INDEX_CHUNKS);
    let index = host.store_status().index_memory.allocated_bytes;
    assert!(index > TIGHT_METADATA_BYTES, "index uses {index} bytes");
    let mut local = attach(&mut host, 2);
    for chunk in [0, INDEX_CHUNKS / 2, INDEX_CHUNKS - 1] {
        read_at(
            &mut local,
            chunk as u64,
            (chunk * BLOCK_SIZE) as u64,
            &chunk_pattern(chunk),
        );
    }
    drop(local);
    shutdown(host);
}
