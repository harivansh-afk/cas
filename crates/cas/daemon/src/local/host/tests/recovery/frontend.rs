use super::*;
use crate::inflight::{Carrier, Identity as CarrierIdentity};
use std::os::{fd::AsRawFd, unix::net::UnixStream};
use vhost::VhostBackend;
use vhost::vhost_user::message::{VhostUserInflight, VhostUserProtocolFeatures};
use vhost::vhost_user::{Frontend, VhostUserFrontend};

#[test]
fn reference_roots_cannot_export_a_cold_recovery_carrier() {
    let root = tempfile::tempdir().unwrap();
    let mut host = create(root.path(), 1, Arc::new(Resources::default()));
    assert!(
        host.attach_cold(
            [2; 16],
            Duration::from_secs(60),
            crate::fault::Fault::new(None)
        )
        .is_err()
    );
    // Rejection leaves the ordinary embedding attachment available.
    let mut local = attach(&mut host, 2);
    read(&mut local, 0, &[0; BLOCK_SIZE]);
    drop(local);
    shutdown(host);
}

#[test]
#[ignore = "requires the exclusive XFS allocation fixture"]
fn first_cold_get_exports_the_recovered_epoch_without_an_extra_rotation() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    setup(root.path(), &resources, SnapshotFixture::Absent);
    let inspected = scan(root.path(), &resources).unwrap();
    let limits = physical_limits(&inspected);
    let recovered = inspected
        .require(Prefixes::Cold)
        .unwrap()
        .recover_cold(limits)
        .unwrap();
    let mut host = recovered
        .into_host(1024 * MAX_REQUEST_BYTES as u64)
        .unwrap();
    let status = host
        .images
        .iter()
        .flatten()
        .find(|image| image.image == [2; 16])
        .unwrap()
        .log
        .status();
    let before = files(root.path());
    let backend = host
        .attach_cold(
            [2; 16],
            Duration::from_secs(60),
            crate::fault::Fault::new(None),
        )
        .unwrap();
    // Transport/report files stay outside the governed XFS allocation domain.
    let transport = tempfile::tempdir_in("/tmp").unwrap();
    let socket = transport.path().join("image.sock");
    let report_path = transport.path().join("report.json");
    let report = fs::File::create(&report_path).unwrap();
    let server_socket = socket.clone();
    let (finished, result) = mpsc::channel();
    let server = thread::spawn(move || {
        finished
            .send(crate::service::serve(backend, &server_socket, report))
            .unwrap();
    });
    let mut frontend = connect(&socket);
    let (message, file) = frontend
        .get_inflight_fd(&VhostUserInflight {
            mmap_size: 0,
            mmap_offset: 0,
            num_queues: 4,
            queue_size: 256,
        })
        .unwrap();
    let carrier = Carrier::attach(
        file.try_clone().unwrap(),
        &message,
        CarrierIdentity {
            store: STORE.store,
            image: [2; 16],
            epoch: status.epoch,
            attachment: status.epoch,
        },
        IMAGE_BYTES,
    )
    .unwrap();
    assert_eq!(carrier.published(), status.published);
    assert_eq!(files(root.path()), before);
    frontend
        .set_inflight_fd(&message, file.as_raw_fd())
        .unwrap();
    frontend.get_features().unwrap(); // Wait for SET handling before sampling.
    assert_eq!(files(root.path()), before);
    drop((carrier, file, frontend));
    result
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    server.join().unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(report["connection_ok"], true);
    assert_eq!(report["inflight"]["active"], false);
    shutdown(host);
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

fn retained(
    root: &Path,
    resources: &Arc<Resources>,
    timeout: Duration,
) -> super::super::super::recovery::RetainedHost {
    let inspected = scan(root, resources).unwrap();
    let limits = physical_limits(&inspected);
    super::super::super::recovery::RetainedHost::start(
        inspected,
        limits,
        1024 * MAX_REQUEST_BYTES as u64,
        crate::deadline::Deadline::after(timeout),
    )
    .unwrap()
}

#[test]
fn retained_collection_does_not_repair_before_all_inputs_and_abandonment_fails_peers() {
    use crate::backend::recovery::testing::Frontend as RetainedFrontend;
    for abandon in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let resources = Arc::new(Resources::default());
        setup(root.path(), &resources, SnapshotFixture::Absent);
        let before = files(root.path());
        let timeout = if abandon {
            Duration::from_secs(2)
        } else {
            Duration::from_millis(100)
        };
        let mut host = retained(root.path(), &resources, timeout);
        let mut first = RetainedFrontend::write(
            host.attach([2; 16], crate::fault::Fault::default())
                .unwrap(),
            2,
        );
        let second = host
            .attach([3; 16], crate::fault::Fault::default())
            .unwrap();
        assert!(!first.activate().unwrap());
        assert_eq!(first.status(), 0xff);
        assert_eq!(first.used(), 0);
        assert_eq!(files(root.path()), before);
        if abandon {
            drop(second);
        } else {
            std::thread::sleep(Duration::from_millis(120));
            drop(second);
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if first.activate().is_err() {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(first.status(), 0xff);
        assert_eq!(first.used(), 0);
        assert_eq!(files(root.path()), before);
        assert!(host.host().is_err());
        drop((first, host));
        while resources.metadata.usage().current.bytes != 0 {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }
}

#[test]
#[ignore = "requires the exclusive XFS allocation fixture"]
fn retained_frontends_share_one_recovery_barrier_and_keep_their_carriers() {
    use crate::backend::recovery::testing::Frontend as RetainedFrontend;
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    setup(root.path(), &resources, SnapshotFixture::Present);
    let before = files(root.path());
    let mut host = retained(root.path(), &resources, Duration::from_secs(60));
    let mut first = RetainedFrontend::write(
        host.attach([2; 16], crate::fault::Fault::default())
            .unwrap(),
        2,
    );
    let mut second = RetainedFrontend::write(
        host.attach([3; 16], crate::fault::Fault::default())
            .unwrap(),
        3,
    );
    assert!(!first.activate().unwrap());
    assert_eq!(first.status(), 0xff);
    assert_eq!(files(root.path()), before);
    first.enable_unused_queue();
    assert!(!first.activate().unwrap());
    assert!(!first.carrier.queue_initialized(1).unwrap());
    assert!(!second.activate().unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    for frontend in [&mut first, &mut second] {
        while !frontend.activate().unwrap() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(frontend.status(), 0);
        assert_eq!(frontend.used(), 1);
        assert_eq!(frontend.carrier.published(), 1);
    }
    assert!(first.carrier.queue_initialized(1).unwrap());
    read(first.local(), 0, &[2; BLOCK_SIZE]);
    read(second.local(), 0, &[3; BLOCK_SIZE]);
    drop((first, second));
    loop {
        if let Some(host) = host.host().unwrap() {
            match host.shutdown() {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
                Err(error) => panic!("shutdown: {error}"),
            }
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    drop(host);
    assert_eq!(resources.metadata.usage().current.bytes, 0);
    let inspected = scan(root.path(), &resources).unwrap();
    let limits = physical_limits(&inspected);
    let recovered = inspected
        .require(Prefixes::Cold)
        .unwrap()
        .recover_cold(limits)
        .unwrap();
    let mut host = recovered
        .into_host(1024 * MAX_REQUEST_BYTES as u64)
        .unwrap();
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    read(&mut first, 0, &[2; BLOCK_SIZE]);
    read(&mut second, 0, &[3; BLOCK_SIZE]);
    drop((first, second));
    shutdown(host);
    assert_eq!(resources.metadata.usage().current.bytes, 0);
}

#[test]
fn retained_captured_queue_changes_fail_before_repair() {
    use crate::backend::recovery::testing::Frontend as RetainedFrontend;
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    setup(root.path(), &resources, SnapshotFixture::Absent);
    let before = files(root.path());
    let mut host = retained(root.path(), &resources, Duration::from_secs(2));
    let mut first = RetainedFrontend::write(
        host.attach([2; 16], crate::fault::Fault::default())
            .unwrap(),
        2,
    );
    assert!(!first.activate().unwrap());
    assert!(first.change_captured_queue().is_err());
    assert!(first.backend.failure().is_some());
    assert_eq!(first.status(), 0xff);
    assert_eq!(first.used(), 0);
    drop((first, host));
    let deadline = Instant::now() + Duration::from_secs(1);
    while resources.metadata.usage().current.bytes != 0 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(files(root.path()), before);
}

pub(super) fn connect(socket: &Path) -> Frontend {
    let deadline = Instant::now() + Duration::from_secs(5);
    let stream = loop {
        match UnixStream::connect(socket) {
            Ok(stream) => break stream,
            Err(error)
                if Instant::now() < deadline
                    && matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) =>
            {
                thread::sleep(Duration::from_millis(1))
            }
            Err(error) => panic!("frontend connection: {error}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut frontend = Frontend::from_stream(stream, 4);
    frontend.set_owner().unwrap();
    let features = frontend.get_features().unwrap();
    frontend.set_features(features).unwrap();
    let protocol = frontend.get_protocol_features().unwrap();
    assert!(protocol.contains(VhostUserProtocolFeatures::INFLIGHT_SHMFD));
    frontend.set_protocol_features(protocol).unwrap();
    frontend
}

#[test]
#[ignore = "requires the exclusive XFS allocation fixture"]
fn retained_zero_variants_replace_written_data_without_gather_and_survive_compaction() {
    use crate::backend::recovery::testing::Frontend as RetainedFrontend;
    use virtio_bindings::bindings::virtio_blk::{VIRTIO_BLK_T_DISCARD, VIRTIO_BLK_T_WRITE_ZEROES};
    for (kind, flags) in [
        (VIRTIO_BLK_T_WRITE_ZEROES, 0),
        (VIRTIO_BLK_T_WRITE_ZEROES, 1),
        (VIRTIO_BLK_T_DISCARD, 0),
    ] {
        let root = tempfile::tempdir().unwrap();
        let resources = Arc::new(Resources::default());
        setup(root.path(), &resources, SnapshotFixture::Present);
        let inspected = scan(root.path(), &resources).unwrap();
        let limits = physical_limits(&inspected);
        let mut host = inspected
            .require(Prefixes::Cold)
            .unwrap()
            .recover_cold(limits)
            .unwrap()
            .into_host(1024 * MAX_REQUEST_BYTES as u64)
            .unwrap();
        for image in 2..4 {
            let mut local = attach(&mut host, image);
            write(&mut local, 0, 0, &[0x55; BLOCK_SIZE]);
            read(&mut local, 1, &[0x55; BLOCK_SIZE]);
        }
        shutdown(host);
        let mut host = retained(root.path(), &resources, Duration::from_secs(60));
        let mut first = RetainedFrontend::zero_after_write(
            host.attach([2; 16], crate::fault::Fault::default())
                .unwrap(),
            kind,
            flags,
        );
        let mut second = RetainedFrontend::zero_after_write(
            host.attach([3; 16], crate::fault::Fault::default())
                .unwrap(),
            kind,
            flags,
        );
        assert!(!first.activate().unwrap());
        assert!(!second.activate().unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        for frontend in [&mut first, &mut second] {
            while !frontend.activate().unwrap() {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(frontend.status(), 0);
            assert_eq!(frontend.used(), 2);
            assert_eq!(frontend.carrier.published(), 2);
            let report = frontend.backend.report(0, true);
            assert_eq!(report["inflight"]["replayed_mutations"], 1);
            assert_eq!(report["inflight"]["replay_copy_bytes"], 0);
            assert_eq!(report["zeroes"], 1);
            read(frontend.local(), 2, &[0; BLOCK_SIZE]);
            drained(frontend.local(), 2);
            read(frontend.local(), 3, &[0; BLOCK_SIZE]);
        }
        drop((first, second));
        loop {
            if let Some(host) = host.host().unwrap() {
                match host.shutdown() {
                    Ok(()) => break,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
                    Err(error) => panic!("shutdown: {error}"),
                }
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        drop(host);
        assert_eq!(resources.metadata.usage().current.bytes, 0);
        let inspected = scan(root.path(), &resources).unwrap();
        let limits = physical_limits(&inspected);
        let mut host = inspected
            .require(Prefixes::Cold)
            .unwrap()
            .recover_cold(limits)
            .unwrap()
            .into_host(1024 * MAX_REQUEST_BYTES as u64)
            .unwrap();
        for image in 2..4 {
            let mut local = attach(&mut host, image);
            read(&mut local, 0, &[0; BLOCK_SIZE]);
        }
        shutdown(host);
        assert_eq!(resources.metadata.usage().current.bytes, 0);
    }
}
