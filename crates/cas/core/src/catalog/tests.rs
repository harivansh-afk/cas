use super::*;
use crate::{
    BLOCK_SIZE,
    budget::Amount,
    direct::faults::{self, Fault},
    manifest::{
        file::{Identity, Manifest},
        format::{Commit, Root},
    },
};
use std::{collections::BTreeMap, path::Path};

const STORE: Id = [1; 16];

fn budget(bytes: usize) -> Arc<Budget> {
    Budget::new(Amount { bytes, requests: 0 })
}

fn metadata() -> Arc<Budget> {
    budget(1024 * 1024)
}

fn image(id: u8) -> Entry {
    Entry {
        id: [id; 16],
        kind: Kind::Image {
            image_bytes: 512 * BLOCK_SIZE as u64,
        },
    }
}

fn snapshot(id: u8) -> Entry {
    Entry {
        id: [id; 16],
        kind: Kind::Snapshot(SnapshotKey {
            commit: Commit {
                store: STORE,
                image: [2; 16],
                generation: 11,
                durable: 91,
                image_bytes: 512 * BLOCK_SIZE as u64,
                root: Root {
                    offset: 8192,
                    height: 1,
                },
            },
            end: 16384,
        }),
    }
}

fn create(root: &Path, metadata: Arc<Budget>) -> Catalog {
    Catalog::create(
        Tickets::open(root, Arc::clone(&metadata)).unwrap(),
        STORE,
        metadata,
    )
    .unwrap()
}

fn inspect(root: &Path) -> Inspection {
    Catalog::inspect(Tickets::open(root, metadata()).unwrap(), STORE, metadata()).unwrap()
}

fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(root.join(DIRECTORY))
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().into_string().unwrap(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

#[test]
fn membership_round_trips_across_pages_and_snapshot_outlives_source() {
    let root = tempfile::tempdir().unwrap();
    let mut catalog = create(root.path(), metadata());
    assert!(catalog.contents().is_empty());
    let mut oracle = BTreeMap::new();
    for entry in (2..70).rev().map(image).chain([snapshot(90)]) {
        let plan = catalog.prepare(Change::Insert(entry)).unwrap();
        catalog.publish(plan).unwrap();
        oracle.insert(entry.id, entry);
        assert_eq!(
            catalog.contents().entries().collect::<Vec<_>>(),
            oracle.values().copied().collect::<Vec<_>>()
        );
    }
    let generation = catalog.contents().generation();
    let before = files(root.path());
    assert!(catalog.prepare(Change::Insert(image(2))).is_err());
    assert!(catalog.prepare(Change::Remove([99; 16])).is_err());
    assert!(!catalog.failed());
    assert_eq!(files(root.path()), before);
    drop(catalog);
    let inspected = inspect(root.path());
    assert_eq!(inspected.contents().store(), STORE);
    assert_eq!(inspected.contents().generation(), generation);
    let mut catalog = inspected.recover().unwrap();
    for id in (2..70).map(|id| [id; 16]) {
        catalog
            .publish(catalog.prepare(Change::Remove(id)).unwrap())
            .unwrap();
        oracle.remove(&id);
        assert_eq!(catalog.contents().len(), oracle.len());
    }
    assert_eq!(
        catalog.contents().entries().collect::<Vec<_>>(),
        [snapshot(90)]
    );
    assert_eq!(catalog.contents().get([2; 16]), None);
    assert_eq!(catalog.contents().get([90; 16]), Some(snapshot(90)));
    assert_eq!(files(root.path()).len(), 1);
    drop(catalog);
    let inspected = inspect(root.path());
    assert_eq!(
        inspected.contents().entries().collect::<Vec<_>>(),
        [snapshot(90)]
    );
    let mut catalog = inspected.recover().unwrap();
    catalog
        .publish(catalog.prepare(Change::Remove([90; 16])).unwrap())
        .unwrap();
    drop(catalog);
    assert!(inspect(root.path()).contents().is_empty());
}

#[test]
fn metadata_overlap_is_charged_and_denial_precedes_output() {
    let root = tempfile::tempdir().unwrap();
    assert!(
        Catalog::create(
            Tickets::open(root.path(), metadata()).unwrap(),
            STORE,
            budget(BLOCK_SIZE - 1)
        )
        .is_err()
    );
    assert!(!root.path().join(DIRECTORY).exists());
    let account = budget(2 * BLOCK_SIZE);
    let mut catalog = create(root.path(), Arc::clone(&account));
    assert_eq!(account.usage().current.bytes, BLOCK_SIZE);
    let plan = catalog.prepare(Change::Insert(image(2))).unwrap();
    assert_eq!(account.usage().current.bytes, 2 * BLOCK_SIZE);
    let before = files(root.path());
    assert!(catalog.prepare(Change::Insert(image(3))).is_err());
    assert_eq!(files(root.path()), before);
    assert!(!catalog.failed());
    drop(plan);
    catalog
        .publish(catalog.prepare(Change::Insert(image(3))).unwrap())
        .unwrap();
    assert_eq!(account.usage().current.bytes, BLOCK_SIZE);
    drop(catalog);
    assert_eq!(account.usage().current.bytes, 0);
    let _inspection = Catalog::inspect(
        Tickets::open(root.path(), metadata()).unwrap(),
        STORE,
        budget(BLOCK_SIZE),
    )
    .unwrap();
}

#[test]
fn stale_foreign_and_dropped_preparations_preserve_actual_directory_exclusion() {
    let root = tempfile::tempdir().unwrap();
    let mut catalog = create(root.path(), metadata());
    let stale = catalog.prepare(Change::Insert(image(2))).unwrap();
    catalog
        .publish(catalog.prepare(Change::Insert(image(3))).unwrap())
        .unwrap();
    let other_root = tempfile::tempdir().unwrap();
    let other = create(other_root.path(), metadata());
    let foreign = other.prepare(Change::Insert(image(4))).unwrap();
    let before = files(root.path());
    for plan in [stale, foreign] {
        assert!(catalog.publish(plan).is_err());
        assert!(!catalog.failed());
        assert_eq!(files(root.path()), before);
    }
    let tickets = Arc::clone(&catalog.owner._tickets);
    assert!(Catalog::inspect(Arc::clone(&tickets), STORE, metadata()).is_err());
    let plan = catalog.prepare(Change::Insert(image(5))).unwrap();
    drop(catalog);
    assert!(Catalog::inspect(Arc::clone(&tickets), STORE, metadata()).is_err());
    drop(tickets);
    assert!(Tickets::open(root.path(), metadata()).is_err());
    drop(plan);
    inspect(root.path());
}

#[test]
fn publication_faults_retain_exact_output_and_require_explicit_recovery() {
    for fault in [
        Fault::Allocate,
        Fault::Write,
        Fault::ShortWrite,
        Fault::FileSync,
        Fault::Rename,
        Fault::DirectorySync,
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut catalog = create(root.path(), metadata());
        for entry in (2..33).map(image) {
            catalog
                .publish(catalog.prepare(Change::Insert(entry)).unwrap())
                .unwrap();
        }
        let old = files(root.path());
        let generation = catalog.contents().generation();
        let plan = catalog.prepare(Change::Insert(image(33))).unwrap();
        let next = plan.contents.bytes().to_vec();
        assert!(next.len() > BLOCK_SIZE); // ShortWrite must persist a proper prefix.
        faults::inject(fault);
        assert!(catalog.publish(plan).is_err());
        assert!(catalog.failed());
        assert_eq!(catalog.contents().generation(), generation);
        assert!(catalog.prepare(Change::Remove([2; 16])).is_err());
        let retained = files(root.path());
        if fault == Fault::DirectorySync {
            assert_eq!(retained.len(), 1);
            assert_eq!(retained[NAME], next);
        } else {
            assert_eq!(retained[NAME], old[NAME]);
            assert_eq!(retained.len(), 2);
            let temporary = retained
                .iter()
                .find(|(name, _)| name.as_str() != NAME)
                .unwrap()
                .1;
            assert_eq!(
                temporary.as_slice(),
                match fault {
                    Fault::Allocate | Fault::Write => &[],
                    Fault::ShortWrite => &next[..BLOCK_SIZE],
                    _ => next.as_slice(),
                }
            );
        }
        drop(catalog);
        let inspected = inspect(root.path());
        let selected_new = fault == Fault::DirectorySync;
        assert_eq!(inspected.contents().get([33; 16]).is_some(), selected_new);
        assert_eq!(files(root.path()), retained);
        let mut recovered = inspected.recover().unwrap();
        assert_eq!(files(root.path()), retained);
        if !selected_new {
            recovered
                .publish(recovered.prepare(Change::Insert(image(34))).unwrap())
                .unwrap();
            let retried = files(root.path());
            for (name, bytes) in retained.iter().filter(|(name, _)| name.as_str() != NAME) {
                assert_eq!(&retried[name], bytes); // Retry cannot overwrite an orphan.
            }
        }
    }
}

#[test]
fn inspection_never_adopts_pending_files_or_repairs_corruption() {
    let root = tempfile::tempdir().unwrap();
    let mut catalog = create(root.path(), metadata());
    let plan = catalog.prepare(Change::Insert(snapshot(90))).unwrap();
    faults::inject(Fault::Rename);
    assert!(catalog.publish(plan).is_err());
    drop(catalog);
    let original = files(root.path());
    let canonical = &original[NAME];
    let path = root.path().join(DIRECTORY).join(NAME);
    let mut corrupt = canonical.clone();
    corrupt[32] ^= 1;
    let mut suffix = canonical.clone();
    suffix.extend_from_slice(&[0; BLOCK_SIZE]);
    for bytes in [
        corrupt,
        suffix,
        canonical[..BLOCK_SIZE - 1].to_vec(),
        vec![],
    ] {
        fs::write(&path, bytes).unwrap();
        let damaged = files(root.path());
        let tickets = Tickets::open(root.path(), metadata()).unwrap();
        assert!(Catalog::inspect(tickets, STORE, metadata()).is_err());
        assert_eq!(files(root.path()), damaged);
    }
    fs::remove_file(&path).unwrap();
    assert!(
        Catalog::inspect(
            Tickets::open(root.path(), metadata()).unwrap(),
            STORE,
            metadata()
        )
        .is_err()
    );
    fs::write(&path, canonical).unwrap();
    assert!(inspect(root.path()).contents().is_empty()); // Complete pending snapshot was never selected.
    let tickets = Tickets::open(root.path(), metadata()).unwrap();
    assert!(Catalog::inspect(Arc::clone(&tickets), [9; 16], metadata()).is_err());
    faults::inject(Fault::Read);
    assert!(Catalog::inspect(tickets, STORE, metadata()).is_err());
    assert_eq!(files(root.path()), original);
}

#[test]
fn recovery_sync_failures_preserve_inspected_files_and_locks() {
    for fault in [Fault::FileSync, Fault::DirectorySync] {
        let root = tempfile::tempdir().unwrap();
        drop(create(root.path(), metadata()));
        let before = files(root.path());
        let inspected = inspect(root.path());
        assert!(Tickets::open(root.path(), metadata()).is_err());
        faults::inject(fault);
        assert!(inspected.recover().is_err());
        assert_eq!(files(root.path()), before);
        inspect(root.path()).recover().unwrap();
    }
}

#[test]
fn dependency_failure_does_not_repair_a_newly_selected_catalog() {
    let root = tempfile::tempdir().unwrap();
    let image_path = root.path().join("dependency");
    fs::create_dir(&image_path).unwrap();
    let identity = Identity {
        store: STORE,
        image: [2; 16],
        image_bytes: 512 * BLOCK_SIZE as u64,
    };
    drop(Manifest::create(&image_path, identity, metadata()).unwrap());
    let mut catalog = create(root.path(), metadata());
    let plan = catalog.prepare(Change::Insert(image(2))).unwrap();
    faults::inject(Fault::DirectorySync);
    assert!(catalog.publish(plan).is_err());
    drop(catalog);
    let manifest_path = image_path.join("manifest.v2");
    let mut manifest = fs::read(&manifest_path).unwrap();
    manifest[BLOCK_SIZE] ^= 1;
    fs::write(&manifest_path, &manifest).unwrap();
    let before = files(root.path());
    let inspected = inspect(root.path());
    assert_eq!(inspected.contents().get([2; 16]), Some(image(2)));
    assert!(Manifest::inspect(&image_path, identity, 0, metadata(), |_| Ok(())).is_err());
    drop(inspected); // Coordinator refuses recover() after dependency failure.
    assert_eq!(files(root.path()), before);
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest);
    assert!(!image_path.join("rejected").exists());
}

#[test]
fn publication_retains_locks_and_returns_only_after_directory_sync() {
    use std::{sync::mpsc, thread, time::Duration};
    for point in [Fault::FileSync, Fault::Rename, Fault::DirectorySync] {
        let root = tempfile::tempdir().unwrap();
        let mut catalog = create(root.path(), metadata());
        let tickets = Arc::clone(&catalog.owner._tickets);
        let plan = catalog.prepare(Change::Insert(image(2))).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            faults::pause_before(point, entered_tx, resume_rx);
            catalog.publish(plan).unwrap();
            done_tx.send(catalog).unwrap();
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(done_rx.try_recv().is_err());
        assert!(Catalog::inspect(Arc::clone(&tickets), STORE, metadata()).is_err());
        let bytes = files(root.path());
        let generation = crate::encoding::u64_at(&bytes[NAME], 32);
        assert_eq!(
            generation,
            if point == Fault::DirectorySync { 2 } else { 1 }
        );
        resume_tx.send(()).unwrap();
        let catalog = done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(catalog.contents().generation(), 2);
        worker.join().unwrap();
        drop((catalog, tickets));
        assert_eq!(inspect(root.path()).contents().get([2; 16]), Some(image(2)));
    }
}

#[test]
fn complete_initial_catalog_is_prepared_before_publication() {
    let root = tempfile::tempdir().unwrap();
    let metadata = metadata();
    let entries = [image(2), image(3), snapshot(4)];
    let initial = Initial::prepare(STORE, entries.into_iter(), Arc::clone(&metadata)).unwrap();
    assert!(fs::read_dir(root.path()).unwrap().next().is_none());
    assert_eq!(initial.output_bytes(), BLOCK_SIZE);
    assert_eq!(initial.contents().generation(), 1);
    let catalog = initial
        .publish(Tickets::open(root.path(), Arc::clone(&metadata)).unwrap())
        .unwrap();
    assert_eq!(catalog.contents().entries().collect::<Vec<_>>(), entries);
    drop(catalog);
    let inspected = inspect(root.path());
    assert_eq!(inspected.contents().generation(), 1);
    assert_eq!(inspected.contents().entries().collect::<Vec<_>>(), entries);
    drop(inspected);
    assert_eq!(metadata.usage().current, Amount::default());
    for entries in [
        [image(2), image(2)],
        [image(3), image(2)],
        [image(0), image(2)],
    ] {
        assert!(Initial::prepare(STORE, entries.into_iter(), Arc::clone(&metadata)).is_err());
        assert_eq!(metadata.usage().current, Amount::default());
    }
    assert!(Initial::prepare(STORE, [image(2)].into_iter(), budget(BLOCK_SIZE - 1)).is_err());
}
