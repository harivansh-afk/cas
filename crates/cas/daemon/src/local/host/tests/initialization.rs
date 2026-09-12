use super::*;
use crate::{deadline::Deadline, initialize};
use cas_core::catalog::{Catalog, Kind};

fn config(root: &Path) -> initialize::Config {
    initialize::Config {
        root: root.to_owned(),
        store: Config {
            segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
            ..STORE
        },
        append: append::Limits::default(),
        staging_bytes: 1024 * MAX_REQUEST_BYTES as u64,
        images: [3, 2]
            .map(|id| initialize::Image {
                id: [id; 16],
                bytes: IMAGE_BYTES,
            })
            .into(),
    }
}

fn run(config: initialize::Config, resources: &Arc<Resources>) -> io::Result<serde_json::Value> {
    initialize::initialize(
        config,
        Arc::clone(resources),
        Deadline::after(Duration::from_secs(60)),
    )
}

#[test]
fn initialization_rejects_invalid_membership_geometry_and_existing_output_before_io() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let invalid = |change: fn(&mut initialize::Config)| {
        let mut config = config(root.path());
        change(&mut config);
        assert!(run(config, &resources).is_err());
        assert!(fs::read_dir(root.path()).unwrap().next().is_none());
        assert_eq!(resources.metadata.usage().current, Amount::default());
    };
    invalid(|config| config.images.clear());
    invalid(|config| config.images[1].id = config.images[0].id);
    invalid(|config| config.images[1].id = [0; 16]);
    invalid(|config| config.images[1].bytes += 1);
    invalid(|config| config.images[1].bytes = 0);
    invalid(|config| config.store.store = [0; 16]);
    invalid(|config| config.store.segment_bytes += 1);
    invalid(|config| config.store.segment_bytes = MAX_REQUEST_BYTES as u64);
    invalid(|config| config.staging_bytes = config.store.segment_bytes * 2);
    invalid(|config| config.append.staging_bytes = config.store.segment_bytes);
    invalid(|config| config.append.intervals = 1);
    fs::write(root.path().join("evidence"), b"preserve").unwrap();
    assert!(run(config(root.path()), &resources).is_err());
    assert_eq!(fs::read(root.path().join("evidence")).unwrap(), b"preserve");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

#[test]
#[ignore = "requires the isolated XFS allocation domain"]
fn initialized_catalog_is_complete_and_both_images_survive_cold_recovery() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let initial = config(root.path());
    let store = initial.store;
    let report = run(initial, &resources).unwrap();
    assert_eq!(report["catalog_generation"], 1);
    assert_eq!(report["images"], 2);
    assert_eq!(report["physical"]["promised"], 0);
    assert_eq!(resources.metadata.usage().current, Amount::default());
    let inspected = crate::recovery::Inspection::scan(
        root.path(),
        crate::recovery::Config {
            store,
            append: append::Limits::default(),
        },
        Arc::clone(&resources),
    )
    .unwrap();
    assert_eq!(inspected.contents().generation(), 1);
    assert_eq!(
        inspected
            .contents()
            .entries()
            .map(|e| e.id)
            .collect::<Vec<_>>(),
        [[2; 16], [3; 16]]
    );
    assert!(inspected.contents().entries().all(|entry| entry.kind
        == Kind::Image {
            image_bytes: IMAGE_BYTES
        }));
    assert!(
        inspected.images().all(|(_, status)| status.published == 0
            && status.durable == 0
            && status.compacted == 0)
    );
    let limits = cas_core::space::Limits::new(
        inspected.observation().unwrap().capacity(),
        store.segment_bytes,
        cas_core::manifest::tree::MAX_TRANSACTION_BYTES as u64,
    )
    .unwrap();
    let mut host = inspected
        .require(crate::recovery::Prefixes::Cold)
        .unwrap()
        .recover_cold(limits)
        .unwrap()
        .into_host(1024 * MAX_REQUEST_BYTES as u64)
        .unwrap();
    for id in [2, 3] {
        let event = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap();
        let mut local = host.local([id; 16], &event).unwrap();
        read(&mut local, 0, &[0; BLOCK_SIZE]);
        drop(local);
    }
    shutdown(host);
    assert_eq!(resources.metadata.usage().current, Amount::default());
    let before = recovery::files(root.path());
    assert!(run(config(root.path()), &resources).is_err());
    assert_eq!(recovery::files(root.path()), before);
}

#[test]
#[ignore = "requires the isolated XFS allocation domain"]
fn initialization_metadata_failure_retains_dependencies_without_publishing_catalog() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources {
        metadata: Budget::new(Amount {
            bytes: 128 * 1024,
            requests: 0,
        }),
        ..Resources::default()
    });
    assert!(run(config(root.path()), &resources).is_err());
    assert!(path(root.path(), 2).join("manifest.v2").is_file());
    assert!(!root.path().join("catalog").exists());
    assert_eq!(resources.metadata.usage().current, Amount::default());
    let before = recovery::files(root.path());
    let fresh = Arc::new(Resources::default());
    assert!(run(config(root.path()), &fresh).is_err());
    assert_eq!(recovery::files(root.path()), before);
    assert!(
        Catalog::inspect(
            Tickets::open(root.path(), Arc::clone(&fresh.metadata)).unwrap(),
            STORE.store,
            Arc::clone(&fresh.metadata)
        )
        .is_err()
    );
}
