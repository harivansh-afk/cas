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
    let deadline = Instant::now() + Duration::from_secs(5);
    let stream = loop {
        match UnixStream::connect(&socket) {
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
