use super::super::recovery::{Checked, Config as OpenConfig, Inspection, Prefix, Prefixes};
use super::*;
use cas_core::catalog::{Catalog, Change, Entry, Kind as EntryKind};
use std::collections::BTreeMap;
use std::io::Write;
mod frontend;
mod live;
mod service;

pub(super) enum SnapshotFixture {
    Absent,
    Missing,
    Present,
}

pub(super) fn setup(root: &Path, resources: &Arc<Resources>, snapshot: SnapshotFixture) {
    let config = Config {
        segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
        ..STORE
    };
    let (store, images) = create_images(
        root,
        2,
        resources,
        IMAGE_BYTES,
        append::Limits::default(),
        config,
    );
    let mut catalog = Catalog::create(
        Arc::clone(store.tickets()),
        STORE.store,
        Arc::clone(&resources.metadata),
    )
    .unwrap();
    for image in 2..4 {
        let prepared = catalog
            .prepare(Change::Insert(Entry {
                id: [image; 16],
                kind: EntryKind::Image {
                    image_bytes: IMAGE_BYTES,
                },
            }))
            .unwrap();
        catalog.publish(prepared).unwrap();
    }
    if !matches!(snapshot, SnapshotFixture::Absent) {
        if matches!(snapshot, SnapshotFixture::Present) {
            let directory = root.join("snapshots").join("09".repeat(16));
            fs::create_dir_all(&directory).unwrap();
            drop(
                Snapshot::create(
                    &images[0].1.view().unwrap(),
                    &directory,
                    Arc::clone(&resources.metadata),
                )
                .unwrap(),
            );
        }
        let prepared = catalog
            .prepare(Change::Insert(Entry {
                id: [9; 16],
                kind: EntryKind::Snapshot(images[0].1.view().unwrap().key()),
            }))
            .unwrap();
        catalog.publish(prepared).unwrap();
    }
    drop((catalog, images, store));
    // Earlier dependencies have repairable suffixes. A later rejection must
    // leave those bytes, and the absence of an archive directory, unchanged.
    fs::OpenOptions::new()
        .append(true)
        .open(path(root, 2).join("manifest.v2"))
        .unwrap()
        .write_all(&[0xa5; BLOCK_SIZE])
        .unwrap();
}

pub(super) fn files(root: &Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn walk(path: &Path, files: &mut BTreeMap<std::path::PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, files);
            } else {
                files.insert(path.clone(), fs::read(path).unwrap());
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(root, &mut found);
    found
}

fn open(root: &Path, resources: &Arc<Resources>, prefixes: Prefixes<'_>) -> io::Result<Checked> {
    scan(root, resources)?.require(prefixes)
}

pub(super) fn scan(root: &Path, resources: &Arc<Resources>) -> io::Result<Inspection> {
    Inspection::scan(
        root,
        OpenConfig {
            store: Config {
                segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
                ..STORE
            },
            append: append::Limits::default(),
        },
        Arc::clone(resources),
    )
}

pub(super) fn physical_limits(inspected: &Inspection) -> cas_core::space::Limits {
    cas_core::space::Limits::new(
        inspected.observation().unwrap().capacity(),
        2 * MAX_REQUEST_BYTES as u64,
        cas_core::manifest::tree::MAX_TRANSACTION_BYTES as u64,
    )
    .unwrap()
}

#[test]
fn complete_catalog_inspection_retains_suffixes_and_enforces_all_saved_prefixes() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    setup(root.path(), &resources, SnapshotFixture::Absent);
    let before = files(root.path());
    let inspected = open(root.path(), &resources, Prefixes::Cold).unwrap();
    assert_eq!(inspected.contents().len(), 2);
    assert!(
        inspected
            .images()
            .all(|(required, status)| required.published == 0 && status.published == 0)
    );
    assert_eq!(inspected.report()["repaired"], false);
    assert_eq!(
        inspected.report()["images"][0]["manifest_file_bytes"],
        3 * BLOCK_SIZE
    );
    assert!(Tickets::open(root.path(), Arc::clone(&resources.metadata)).is_err());
    assert_eq!(files(root.path()), before);
    drop(inspected);
    assert_eq!(resources.metadata.usage().current, Amount::default());
    let first = Prefix {
        image: [2; 16],
        published: 0,
    };
    let second = Prefix {
        image: [3; 16],
        published: 0,
    };
    for required in [
        vec![first],
        vec![first, first],
        vec![second, first],
        vec![
            first,
            Prefix {
                image: [9; 16],
                published: 0,
            },
        ],
        vec![
            first,
            Prefix {
                published: 1,
                ..second
            },
        ],
    ] {
        assert!(open(root.path(), &resources, Prefixes::Retained(&required)).is_err());
        assert_eq!(files(root.path()), before);
        assert_eq!(resources.metadata.usage().current, Amount::default());
    }
    drop(
        open(
            root.path(),
            &resources,
            Prefixes::Retained(&[first, second]),
        )
        .unwrap(),
    );
    assert_eq!(files(root.path()), before);
}

#[test]
fn a_missing_snapshot_rejects_the_whole_catalog_before_earlier_image_repair() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    setup(root.path(), &resources, SnapshotFixture::Missing);
    let before = files(root.path());
    assert!(open(root.path(), &resources, Prefixes::Cold).is_err());
    assert_eq!(files(root.path()), before);
    assert_eq!(resources.metadata.usage().current, Amount::default());
    assert!(Tickets::open(root.path(), Arc::clone(&resources.metadata)).is_ok());
}

#[test]
fn cold_recovery_rejects_retained_mode_and_invalid_reserve_without_repair() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    setup(root.path(), &resources, SnapshotFixture::Absent);
    let before = files(root.path());
    for retained in [false, true] {
        let inspected = scan(root.path(), &resources).unwrap();
        assert_eq!(inspected.report()["prefixes_checked"], false);
        let mut limits = physical_limits(&inspected);
        let prefixes: Vec<_> = inspected
            .images()
            .map(|(image, _)| Prefix {
                image,
                published: 0,
            })
            .collect();
        let prefixes = if retained {
            Prefixes::Retained(&prefixes)
        } else {
            limits.reserve -= 1;
            Prefixes::Cold
        };
        assert_eq!(
            inspected
                .require(prefixes)
                .unwrap()
                .recover_cold(limits)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(files(root.path()), before);
        assert_eq!(resources.metadata.usage().current, Amount::default());
    }
}

#[test]
fn last_image_archive_bound_is_checked_before_the_first_repair() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    setup(root.path(), &resources, SnapshotFixture::Absent);
    let last = path(root.path(), 3).join("manifest.v2");
    let inspected = scan(root.path(), &resources).unwrap();
    let limits = physical_limits(&inspected);
    drop(inspected);
    fs::OpenOptions::new()
        .write(true)
        .open(&last)
        .unwrap()
        .set_len(limits.reserve + BLOCK_SIZE as u64)
        .unwrap();
    // Record paths and lengths without reading the intentionally sparse tail.
    let first = fs::read(path(root.path(), 2).join("manifest.v2")).unwrap();
    let inspected = open(root.path(), &resources, Prefixes::Cold).unwrap();
    assert_eq!(
        inspected.recover_cold(limits).err().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(
        fs::read(path(root.path(), 2).join("manifest.v2")).unwrap(),
        first
    );
    assert_eq!(
        fs::metadata(last).unwrap().len(),
        limits.reserve + BLOCK_SIZE as u64
    );
    for image in 2..4 {
        assert!(!path(root.path(), image).join("rejected").exists());
    }
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

#[test]
fn result_table_denial_precedes_all_recovery_output() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    setup(root.path(), &resources, SnapshotFixture::Absent);
    let before = files(root.path());
    let inspected = scan(root.path(), &resources).unwrap();
    let limits = physical_limits(&inspected);
    let held = resources
        .metadata
        .reserve(Amount {
            bytes: 128 * MAX_REQUEST_BYTES - resources.metadata.usage().current.bytes,
            requests: 0,
        })
        .unwrap();
    assert_eq!(
        inspected
            .require(Prefixes::Cold)
            .unwrap()
            .recover_cold(limits)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::OutOfMemory
    );
    assert_eq!(files(root.path()), before);
    drop(held);
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

#[test]
#[ignore = "requires the exclusive XFS allocation fixture"]
fn cold_recovery_archives_suffixes_serves_both_images_and_retains_catalog_ownership() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    setup(root.path(), &resources, SnapshotFixture::Present);
    for restart in 0..2 {
        let inspected = scan(root.path(), &resources).unwrap();
        let epochs: Vec<_> = inspected.images().map(|(_, status)| status.epoch).collect();
        let limits = physical_limits(&inspected);
        let tickets = Arc::clone(&inspected.tickets);
        let recovered = inspected
            .require(Prefixes::Cold)
            .unwrap()
            .recover_cold(limits)
            .unwrap();
        assert_eq!(recovered.contents().len(), 3);
        let mut host = recovered
            .into_host(1024 * MAX_REQUEST_BYTES as u64)
            .unwrap();
        assert!(
            Catalog::inspect(
                Arc::clone(&tickets),
                STORE.store,
                Arc::clone(&resources.metadata)
            )
            .is_err()
        );
        let mut locals: Vec<_> = (2..4).map(|image| attach(&mut host, image)).collect();
        for (index, local) in locals.iter_mut().enumerate() {
            assert!(local.report()["status"]["epoch"].as_u64().unwrap() > epochs[index]);
            let expected = if restart == 0 { 0 } else { index as u8 + 7 };
            read(local, 0, &[expected; BLOCK_SIZE]);
            write(local, 1, 0, &[index as u8 + 7; BLOCK_SIZE]);
            drained(local, restart + 1);
        }
        drop(locals);
        shutdown(host);
        drop(
            Catalog::inspect(
                Arc::clone(&tickets),
                STORE.store,
                Arc::clone(&resources.metadata),
            )
            .unwrap(),
        );
        drop(tickets);
        assert_eq!(resources.metadata.usage().current, Amount::default());
        assert_eq!(resources.compaction.usage().current, Amount::default());
    }
    let archived: Vec<_> = files(root.path())
        .into_iter()
        .filter(|(path, _)| path.components().any(|part| part.as_os_str() == "rejected"))
        .collect();
    assert!(
        archived
            .iter()
            .any(|(_, bytes)| bytes == &vec![0xa5; BLOCK_SIZE])
    );
}
