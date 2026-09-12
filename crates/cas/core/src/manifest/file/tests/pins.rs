use super::*;
use std::collections::BTreeSet;

fn publish(manifest: &mut Manifest, block: u64, hash: u8, durable: u64) -> View {
    let prepared = manifest.prepare(&[change(block, hash)], durable).unwrap();
    manifest.publish(prepared).unwrap();
    manifest.view().unwrap()
}

fn resolve(mut read: Read) -> Option<Hash> {
    let mut scratch = AlignedBuffer::new(BLOCK_SIZE);
    loop {
        match read.state().unwrap() {
            LookupState::Complete(hash) => return hash,
            LookupState::Page { offset, .. } => {
                direct::read_bytes(read.file(), scratch.as_mut_slice(), offset).unwrap();
                read.accept(offset, scratch.as_slice()).unwrap();
            }
        }
    }
}

#[test]
fn roots_include_old_views_and_unfinished_reads_then_release_their_keys() {
    let root = tempfile::tempdir().unwrap();
    let mut manifest = Manifest::create(root.path(), ID, metadata()).unwrap();
    let first = publish(&mut manifest, 0, 11, 1);
    let first_key = first.key();
    let read = first.lookup(0).unwrap();
    drop(first); // Only the unfinished Read now pins this root.
    let second = publish(&mut manifest, 0, 22, 2);
    let second_key = second.key();
    let current = publish(&mut manifest, 0, 33, 3);
    let account = metadata();
    let roots = manifest.pinned_roots(Arc::clone(&account)).unwrap();
    assert_eq!(roots.keys(), [first_key, second_key, current.key()]);
    assert_eq!(account.usage().current.bytes, 3 * size_of::<SnapshotKey>());
    drop(second); // Captured Roots stays conservatively complete.
    let mut marked = BTreeSet::new();
    roots
        .walk(|extent| {
            marked.insert(extent.hash.unwrap());
            Ok(())
        })
        .unwrap();
    assert_eq!(marked, BTreeSet::from([[11; 32], [22; 32], [33; 32]]));
    drop(roots);
    assert_eq!(account.usage().current.bytes, 0);
    assert_eq!(
        manifest.pinned_roots(metadata()).unwrap().keys(),
        [first_key, current.key()]
    );
    assert_eq!(resolve(read), Some([11; 32]));
    assert_eq!(
        manifest.pinned_roots(metadata()).unwrap().keys(),
        [current.key()]
    );
}

#[test]
fn registry_growth_denial_precedes_publication_and_failed_io_exposes_no_new_pin() {
    let root = tempfile::tempdir().unwrap();
    let account = metadata();
    let mut manifest = Manifest::create(root.path(), ID, Arc::clone(&account)).unwrap();
    let mut views = vec![manifest.view().unwrap()];
    let held = account
        .reserve(Amount {
            bytes: 128 * 1024 * 1024 - account.usage().current.bytes,
            requests: 0,
        })
        .unwrap();
    let mut denied = None;
    for durable in 1..=64 {
        let before = bytes(root.path());
        let prepared = manifest
            .prepare_with_metadata(&[change(0, durable as u8)], durable, metadata())
            .unwrap();
        match manifest.publish(prepared) {
            Ok(_) => views.push(manifest.view().unwrap()),
            Err(error) => {
                assert_eq!(error.kind(), io::ErrorKind::OutOfMemory);
                assert!(!manifest.failed());
                assert_eq!(bytes(root.path()), before);
                denied = Some(durable);
                break;
            }
        }
    }
    let durable = denied.expect("finite root capacity cannot grow without credits");
    let expected: Vec<_> = views.iter().map(View::key).collect();
    assert_eq!(manifest.pinned_roots(metadata()).unwrap().keys(), expected);
    assert!(manifest.pinned_roots(budget(0)).is_err());
    drop(held);
    let next = publish(&mut manifest, 0, 199, durable);
    assert_eq!(
        manifest.pinned_roots(metadata()).unwrap().keys().last(),
        Some(&next.key())
    );
    drop((manifest, views, next));
    assert_eq!(account.usage().current.bytes, 0);

    for fault in [Fault::Allocate, Fault::Write, Fault::Sync] {
        let root = tempfile::tempdir().unwrap();
        let account = metadata();
        let mut manifest = Manifest::create(root.path(), ID, Arc::clone(&account)).unwrap();
        let old = publish(&mut manifest, 0, 71, 1);
        let prepared = manifest.prepare(&[change(0, 88)], 2).unwrap();
        faults::inject(fault);
        assert!(manifest.publish(prepared).is_err());
        assert!(manifest.view().is_err());
        assert!(manifest.pinned_roots(metadata()).is_err());
        assert_eq!(
            old.pin.capture(old.file(), metadata()).unwrap().keys(),
            [old.key()]
        );
        assert_eq!(resolve(old.lookup(0).unwrap()), Some([71; 32]));
        drop(manifest);
        assert!(account.usage().current.bytes > 0);
        assert!(inspect(root.path(), 0).is_err());
        drop(old);
        assert_eq!(account.usage().current.bytes, 0);
        inspect(root.path(), 0).unwrap();
    }
}

#[test]
fn concurrent_read_pins_keep_registry_charges_and_actual_file_after_owner_drop() {
    use std::{
        sync::{Barrier, mpsc},
        thread,
        time::Duration,
    };
    let root = tempfile::tempdir().unwrap();
    let account = metadata();
    let mut manifest = Manifest::create(root.path(), ID, Arc::clone(&account)).unwrap();
    let view = publish(&mut manifest, 0, 51, 1);
    let charge = account.usage().current.bytes;
    let allocations = account.usage().admitted;
    let barrier = Arc::new(Barrier::new(17));
    let (ready_tx, ready_rx) = mpsc::channel();
    let threads: Vec<_> = (0..16)
        .map(|_| {
            let view = view.clone();
            let barrier = Arc::clone(&barrier);
            let ready_tx = ready_tx.clone();
            thread::spawn(move || {
                let read = view.lookup(0).unwrap();
                drop(view);
                ready_tx.send(()).unwrap();
                barrier.wait();
                assert_eq!(resolve(read), Some([51; 32]));
            })
        })
        .collect();
    for _ in 0..16 {
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    drop((view, manifest));
    assert_eq!(account.usage().current.bytes, charge);
    assert_eq!(account.usage().admitted, allocations); // Cloning/resumed lookup did not allocate.
    assert!(inspect(root.path(), 0).is_err());
    barrier.wait();
    for worker in threads {
        worker.join().unwrap();
    }
    assert_eq!(account.usage().current.bytes, 0);
    inspect(root.path(), 0).unwrap();
}
