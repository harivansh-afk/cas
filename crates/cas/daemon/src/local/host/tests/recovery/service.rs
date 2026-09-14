use super::*;
use crate::host_service::{self, Config as ServiceConfig, Endpoint, Mode};
use std::os::fd::AsRawFd;
use vhost::{
    VhostBackend,
    vhost_user::{VhostUserFrontend, message::VhostUserInflight},
};

fn config(root: &Path, transport: &Path, mode: Mode) -> ServiceConfig {
    ServiceConfig {
        cache_bytes: Resources::DEFAULT_CACHE_BYTES,
        telemetry: false,
        telemetry_rotate: false,
        root: root.to_owned(),
        store: STORE.store,
        segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
        staging_bytes: 1024 * MAX_REQUEST_BYTES as u64,
        mode,
        pause: None,
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
    let mut bad = super::frontend::connect(&transport.path().join("2.sock"));
    let mut good = super::frontend::connect(&transport.path().join("3.sock"));
    good.get_features().unwrap();
    let invalid = VhostUserInflight {
        num_queues: 3,
        ..inflight()
    };
    assert!(bad.get_inflight_fd(&invalid).is_err());
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
    assert!(result.try_recv().is_err());
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
