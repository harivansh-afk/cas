use super::*;
use crate::{
    budget::Amount,
    direct::faults::{self, Fault},
};
use std::{fs, io::Write, os::unix::fs::FileExt};

const ID: Identity = Identity {
    store: [1; 16],
    image: [2; 16],
    image_bytes: 512 * BLOCK_SIZE as u64,
};

fn budget(bytes: usize) -> Arc<Budget> {
    Budget::new(Amount { bytes, requests: 0 })
}

fn metadata() -> Arc<Budget> {
    budget(128 * 1024 * 1024)
}

fn change(block: u64, value: u8) -> Extent {
    Extent {
        start: block,
        end: block + 1,
        hash: Some([value; 32]),
    }
}

fn inspect(path: &Path, required: u64) -> io::Result<Inspection> {
    Manifest::inspect(path, ID, required, metadata(), |_| Ok(()))
}

fn bytes(path: &Path) -> Vec<u8> {
    fs::read(path.join(NAME)).unwrap()
}

fn assert_unchanged(path: &Path, original: &[u8]) {
    assert_eq!(bytes(path), original);
    assert!(!path.join("rejected").exists());
}

#[test]
fn native_publication_and_reopen_preserve_current_and_historical_images() {
    let dir = tempfile::tempdir().unwrap();
    let memory = metadata();
    let mut manifest = Manifest::create(dir.path(), ID, Arc::clone(&memory)).unwrap();
    let mut oracle = [None; 512];
    let mut previous = (manifest.current(), manifest.end(), oracle);
    for sequence in 1..=96 {
        let start = sequence * 17 % 512;
        let edit = if sequence % 5 == 0 {
            Extent {
                start,
                end: (start + 40).min(512),
                hash: None,
            }
        } else {
            change(start, sequence as u8)
        };
        oracle[edit.start as usize..edit.end as usize].fill(edit.hash);
        let prepared = manifest.prepare(&[edit], sequence).unwrap();
        let old_commit = manifest.current();
        let new_commit = prepared.commit();
        assert_eq!(manifest.current(), old_commit);
        assert_eq!(manifest.publish(prepared).unwrap(), new_commit);
        let mut tree = manifest.tree().unwrap();
        let mut old =
            Tree::new(&manifest.file, previous.0, previous.1, Arc::clone(&memory)).unwrap();
        for block in 0..512 {
            assert_eq!(tree.get(block).unwrap(), oracle[block as usize]);
            assert_eq!(old.get(block).unwrap(), previous.2[block as usize]);
        }
        if sequence == 48 {
            previous = (manifest.current(), manifest.end(), oracle);
        }
    }
    let expected = manifest.current();
    drop(manifest);
    assert_eq!(memory.usage().current.bytes, 0);
    let inspection = inspect(dir.path(), 96).unwrap();
    assert_eq!(inspection.selected().commit, expected);
    let recovered = inspection.recover().unwrap();
    let mut tree = recovered.tree().unwrap();
    for block in 0..512 {
        assert_eq!(tree.get(block).unwrap(), oracle[block as usize]);
    }
}

#[test]
fn failed_write_or_sync_keeps_published_root_and_poisoned_owner() {
    for fault in [Fault::Allocate, Fault::ShortWrite, Fault::Sync] {
        let dir = tempfile::tempdir().unwrap();
        let mut manifest = Manifest::create(dir.path(), ID, metadata()).unwrap();
        let original = manifest.current();
        let prepared = manifest.prepare(&[change(7, 9)], 1).unwrap();
        let next = prepared.commit();
        let start = prepared.offset();
        let planned = prepared.bytes().to_vec();
        faults::inject(fault);
        assert!(manifest.publish(prepared).is_err());
        assert!(manifest.failed());
        assert_eq!(manifest.current(), original);
        assert_eq!(manifest.end(), start);
        assert!(manifest.prepare(&[], 2).is_err());
        assert!(manifest.tree().is_err());
        drop(manifest);
        let inspection = inspect(dir.path(), 0).unwrap();
        let selected = inspection.selected();
        if fault != Fault::Sync {
            assert_eq!(selected.commit, original);
            assert_eq!(
                selected.file_bytes,
                start
                    + if fault == Fault::ShortWrite {
                        BLOCK_SIZE as u64
                    } else {
                        0
                    }
            );
        } else {
            // Process-crash model: completed but unsynced bytes can survive.
            assert_eq!(selected.commit, next);
        }
        let recovered = inspection.recover().unwrap();
        if fault == Fault::ShortWrite {
            let archives = fs::read_dir(dir.path().join("rejected"))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(archives.len(), 1);
            assert_eq!(fs::read(archives[0].path()).unwrap(), planned[..BLOCK_SIZE]);
            assert_eq!(bytes(dir.path()).len() as u64, start);
        }
        assert!(!recovered.failed());
    }
}

#[test]
fn invalid_identity_or_creation_budget_fails_before_creating_a_file() {
    let dir = tempfile::tempdir().unwrap();
    for identity in [
        Identity {
            image: [0; 16],
            ..ID
        },
        Identity {
            store: [0; 16],
            ..ID
        },
        Identity {
            image_bytes: 512,
            ..ID
        },
    ] {
        assert!(Manifest::create(dir.path(), identity, metadata()).is_err());
    }
    let short = budget(BLOCK_SIZE);
    assert!(Manifest::create(dir.path(), ID, Arc::clone(&short)).is_err());
    assert_eq!(short.usage().current.bytes, 0);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    faults::inject(Fault::ShortWrite);
    assert!(Manifest::create(dir.path(), ID, metadata()).is_err());
    let partial = bytes(dir.path());
    assert_eq!(partial.len(), BLOCK_SIZE);
    assert!(inspect(dir.path(), 0).is_err());
    assert_unchanged(dir.path(), &partial);
}

#[test]
fn stale_or_invalid_plans_do_not_write_or_poison_the_owner() {
    let dir = tempfile::tempdir().unwrap();
    let mut manifest = Manifest::create(dir.path(), ID, metadata()).unwrap();
    let first = manifest.prepare(&[change(1, 1)], 1).unwrap();
    let stale = manifest.prepare(&[change(2, 2)], 1).unwrap();
    manifest.publish(first).unwrap();
    let original = bytes(dir.path());
    assert!(manifest.publish(stale).is_err());
    assert!(manifest.prepare(&[change(512, 3)], 2).is_err());
    assert!(!manifest.failed());
    assert_unchanged(dir.path(), &original);
}

#[test]
fn incomplete_metadata_can_fall_back_but_required_d_and_missing_chunks_cannot() {
    let dir = tempfile::tempdir().unwrap();
    let mut manifest = Manifest::create(dir.path(), ID, metadata()).unwrap();
    let original = manifest.current();
    let prepared = manifest.prepare(&[change(7, 9)], 1).unwrap();
    manifest.publish(prepared).unwrap();
    let root = manifest.current().root.offset;
    drop(manifest);
    let complete = bytes(dir.path());
    let missing = Manifest::inspect(dir.path(), ID, 0, metadata(), |_| {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "required chunk missing",
        ))
    });
    assert!(matches!(missing, Err(ref error) if error.kind() == io::ErrorKind::NotFound));
    assert_unchanged(dir.path(), &complete);
    // A valid COMMIT can survive without its new page. Such metadata is incomplete.
    fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join(NAME))
        .unwrap()
        .write_all_at(&[0; BLOCK_SIZE], root)
        .unwrap();
    let incomplete = bytes(dir.path());
    assert!(inspect(dir.path(), 1).is_err());
    assert_unchanged(dir.path(), &incomplete);
    let selected = inspect(dir.path(), 0).unwrap();
    assert_eq!(selected.selected().commit, original);
    assert_eq!(selected.selected().incomplete_commits, 1);
    assert_unchanged(dir.path(), &incomplete);
    let recovered = selected.recover().unwrap();
    assert_eq!(recovered.current(), original);
    assert_eq!(bytes(dir.path()).len(), 2 * BLOCK_SIZE);
}

#[test]
fn io_failures_are_never_incomplete_metadata_or_implicit_repair_permission() {
    let dir = tempfile::tempdir().unwrap();
    let mut manifest = Manifest::create(dir.path(), ID, metadata()).unwrap();
    let prepared = manifest.prepare(&[change(1, 2)], 1).unwrap();
    manifest.publish(prepared).unwrap();
    drop(manifest);
    let original = bytes(dir.path());
    // FILE read, newest COMMIT read, structural tree read, chunk-validation read.
    for successful_reads in 0..4 {
        faults::inject_after(Fault::Read, successful_reads);
        let result = inspect(dir.path(), 0);
        assert!(matches!(result, Err(ref error) if error.raw_os_error() == Some(libc::EIO)));
        assert_unchanged(dir.path(), &original);
    }
    let selected = inspect(dir.path(), 1).unwrap();
    faults::inject(Fault::Sync);
    assert!(selected.recover().is_err());
    assert_unchanged(dir.path(), &original);
    assert_eq!(
        inspect(dir.path(), 1)
            .unwrap()
            .recover()
            .unwrap()
            .current()
            .durable,
        1
    );
}

#[test]
fn catalog_binding_rejects_the_wrong_image_including_clone_history() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = Manifest::create(dir.path(), ID, metadata()).unwrap();
    let mut clone = manifest.current();
    clone.image = [3; 16];
    clone.generation += 1;
    let offset = manifest.end();
    direct::write(&manifest.file, &clone.encode(offset).unwrap(), offset).unwrap();
    drop(manifest);
    let original = bytes(dir.path());
    assert!(inspect(dir.path(), 0).is_err());
    for wrong in [
        Identity {
            store: [4; 16],
            ..ID
        },
        Identity {
            image_bytes: ID.image_bytes * 2,
            ..ID
        },
    ] {
        assert!(Manifest::inspect(dir.path(), wrong, 0, metadata(), |_| Ok(())).is_err());
    }
    assert_unchanged(dir.path(), &original);
    let clone_id = Identity {
        image: clone.image,
        ..ID
    };
    let inspection = Manifest::inspect(dir.path(), clone_id, 0, metadata(), |_| Ok(())).unwrap();
    assert_eq!(inspection.selected().commit, clone);
    inspection.recover().unwrap();
}

#[test]
fn torn_suffixes_are_preserved_byte_for_byte_and_repair_is_repeatable() {
    for tail in [1, 511, 4095, 4096, 4097, 8191, 8192] {
        let dir = tempfile::tempdir().unwrap();
        let manifest = Manifest::create(dir.path(), ID, metadata()).unwrap();
        let expected = manifest.current();
        drop(manifest);
        let suffix = (0..tail).map(|n| (n % 251) as u8).collect::<Vec<_>>();
        fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join(NAME))
            .unwrap()
            .write_all(&suffix)
            .unwrap();
        let original = bytes(dir.path());
        let inspected = inspect(dir.path(), 0).unwrap();
        assert_unchanged(dir.path(), &original);
        faults::inject(Fault::Sync); // The archive and truncation finish first.
        assert!(inspected.recover().is_err());
        assert_eq!(bytes(dir.path()).len(), 2 * BLOCK_SIZE);
        let archives = fs::read_dir(dir.path().join("rejected"))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(archives.len(), 1);
        assert_eq!(fs::read(archives[0].path()).unwrap(), suffix);
        assert_eq!(
            inspect(dir.path(), 0).unwrap().recover().unwrap().current(),
            expected
        );
        assert_eq!(
            fs::read_dir(dir.path().join("rejected")).unwrap().count(),
            1
        );
    }
}

#[test]
fn archive_failure_cannot_discard_the_original_suffix() {
    let dir = tempfile::tempdir().unwrap();
    drop(Manifest::create(dir.path(), ID, metadata()).unwrap());
    fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join(NAME))
        .unwrap()
        .write_all(b"retain this failed transaction")
        .unwrap();
    let original = bytes(dir.path());
    fs::write(dir.path().join("rejected"), b"occupied").unwrap();
    assert!(inspect(dir.path(), 0).unwrap().recover().is_err());
    assert_eq!(bytes(dir.path()), original);
    fs::remove_file(dir.path().join("rejected")).unwrap();
    inspect(dir.path(), 0).unwrap().recover().unwrap();
    assert_eq!(bytes(dir.path()).len(), 2 * BLOCK_SIZE);
}

#[test]
fn file_locks_and_recovery_scratch_remain_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = Manifest::create(dir.path(), ID, metadata()).unwrap();
    assert!(inspect(dir.path(), 0).is_err());
    // A long sequence of orphan pages does not allocate a list during inspection.
    manifest.file.set_len(1024 * BLOCK_SIZE as u64).unwrap();
    drop(manifest);
    let memory = budget(2 * BLOCK_SIZE);
    let inspection = Manifest::inspect(dir.path(), ID, 0, Arc::clone(&memory), |_| Ok(())).unwrap();
    assert_eq!(inspection.selected().scanned_pages, 1023);
    assert_eq!(memory.usage().peak.bytes, 2 * BLOCK_SIZE);
    assert_eq!(memory.usage().current.bytes, 0);
    assert!(inspect(dir.path(), 0).is_err());
    let file = dir.path().join(NAME);
    assert!(direct::open(&file, false).is_err());
    drop(inspection);
    assert!(direct::open(&file, false).is_ok());
    let short = budget(BLOCK_SIZE);
    let original = bytes(dir.path());
    assert!(Manifest::inspect(dir.path(), ID, 0, Arc::clone(&short), |_| Ok(())).is_err());
    assert_eq!(short.usage().current.bytes, 0);
    assert_unchanged(dir.path(), &original);
}
