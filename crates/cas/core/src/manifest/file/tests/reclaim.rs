use super::*;
use std::os::unix::fs::MetadataExt;

fn publish(manifest: &mut Manifest, hash: u8, durable: u64) -> View {
    manifest
        .publish(manifest.prepare(&[change(0, hash)], durable).unwrap())
        .unwrap();
    manifest.view().unwrap()
}

fn current(view: &View, hash: u8) {
    let mut tree = view.tree().unwrap();
    for block in 0..512 {
        assert_eq!(tree.get(block).unwrap(), (block == 0).then_some([hash; 32]));
    }
}

fn populated(path: &Path) -> (Manifest, View) {
    let mut manifest = Manifest::create(path, ID, metadata()).unwrap();
    let old = publish(&mut manifest, 11, 1);
    publish(&mut manifest, 22, 2);
    publish(&mut manifest, 33, 3);
    (manifest, old)
}

fn unchanged_pages(before: &[u8], after: &[u8], retained: &[usize]) {
    assert_eq!(before.len(), after.len());
    for &page in retained {
        let range = page * BLOCK_SIZE..(page + 1) * BLOCK_SIZE;
        assert_eq!(before[range.clone()], after[range]);
    }
}

#[test]
fn sweep_preserves_current_old_views_and_unfinished_reads_then_releases_old_pages() {
    let root = tempfile::tempdir().unwrap();
    let mut manifest = Manifest::create(root.path(), ID, metadata()).unwrap();
    let empty = manifest.view().unwrap();
    let old = publish(&mut manifest, 11, 1);
    let mut read = old.lookup(0).unwrap();
    drop(old);
    publish(&mut manifest, 22, 2);
    publish(&mut manifest, 33, 3);
    let before = bytes(root.path());
    assert_eq!(before.len(), 8 * BLOCK_SIZE); // Initial FILE/COMMIT plus three leaf/COMMIT pairs.
    let result = manifest.reclaim_pages(metadata()).unwrap();
    assert_eq!((result.roots, result.retained_pages), (3, 6));
    assert_eq!(result.punched_logical_bytes, 2 * BLOCK_SIZE as u64);
    let after = bytes(root.path());
    unchanged_pages(&before, &after, &[0, 1, 2, 3, 6, 7]);
    assert!(
        after[4 * BLOCK_SIZE..6 * BLOCK_SIZE]
            .iter()
            .all(|&byte| byte == 0)
    );
    let mut scratch = AlignedBuffer::new(BLOCK_SIZE);
    while let LookupState::Page { offset, .. } = read.state().unwrap() {
        direct::read_bytes(read.file(), scratch.as_mut_slice(), offset).unwrap();
        read.accept(offset, scratch.as_slice()).unwrap();
    }
    assert_eq!(read.state().unwrap(), LookupState::Complete(Some([11; 32])));
    assert_eq!(empty.tree().unwrap().get(0).unwrap(), None);
    current(&manifest.view().unwrap(), 33);
    drop((read, empty));
    let result = manifest.reclaim_pages(metadata()).unwrap();
    assert_eq!((result.roots, result.retained_pages), (1, 3));
    let after = bytes(root.path());
    unchanged_pages(&before, &after, &[0, 6, 7]);
    assert!(
        after[BLOCK_SIZE..6 * BLOCK_SIZE]
            .iter()
            .all(|&byte| byte == 0)
    );
    manifest.reclaim_pages(metadata()).unwrap();
    drop(manifest);
    let recovered = inspect(root.path(), 3).unwrap().recover().unwrap();
    current(&recovered.view().unwrap(), 33);
    assert!(!root.path().join("rejected").exists());
}

#[test]
fn interrupted_mark_punch_and_sync_preserve_every_retained_mapping() {
    for (fault, after_calls) in [
        (Fault::Read, 0),
        (Fault::Map, 0),
        (Fault::Sync, 0),
        (Fault::Punch, 0),
        (Fault::Punch, 1),
        (Fault::Sync, 1),
    ] {
        let root = tempfile::tempdir().unwrap();
        let (mut manifest, old) = populated(root.path());
        let latest = manifest.view().unwrap();
        let before = bytes(root.path());
        faults::inject_after(fault, after_calls);
        assert!(manifest.reclaim_pages(metadata()).is_err());
        assert!(manifest.failed());
        assert!(manifest.view().is_err());
        assert!(manifest.reclaim_pages(metadata()).is_err());
        let after = bytes(root.path());
        unchanged_pages(&before, &after, &[0, 2, 3, 6, 7]);
        if after_calls == 0 {
            assert_eq!(before, after);
        }
        current(&old, 11);
        current(&latest, 33);
        drop((manifest, old, latest));
        let mut recovered = inspect(root.path(), 3).unwrap().recover().unwrap();
        current(&recovered.view().unwrap(), 33);
        recovered.reclaim_pages(metadata()).unwrap();
        current(&recovered.view().unwrap(), 33);
    }
}

#[test]
fn metadata_denial_and_invalid_eof_commit_or_tree_cannot_justify_a_punch() {
    let root = tempfile::tempdir().unwrap();
    let (mut manifest, old) = populated(root.path());
    let before = bytes(root.path());
    let denied = budget(2 * BLOCK_SIZE + 2 * size_of::<SnapshotKey>() - 1);
    assert!(manifest.reclaim_pages(Arc::clone(&denied)).is_err());
    assert_eq!(denied.usage().current.bytes, 0);
    assert!(!manifest.failed());
    assert_eq!(bytes(root.path()), before);
    current(&old, 11);
    drop((manifest, old));

    for damage in 0..4 {
        let root = tempfile::tempdir().unwrap();
        let (mut manifest, old) = populated(root.path());
        let mut damaged = bytes(root.path());
        match damage {
            0 => damaged.extend_from_slice(&[0; BLOCK_SIZE]),
            1 => damaged[0] ^= 1,
            2 => damaged[(manifest.end() - BLOCK_SIZE as u64) as usize] ^= 1,
            _ => damaged[manifest.current().root.offset as usize] ^= 1,
        }
        fs::write(root.path().join(NAME), &damaged).unwrap();
        faults::inject(Fault::Punch);
        assert!(manifest.reclaim_pages(metadata()).is_err());
        assert!(manifest.failed());
        assert!(faults::take(Fault::Punch)); // The punch boundary was never reached.
        assert_eq!(bytes(root.path()), damaged);
        assert!(!root.path().join("rejected").exists());
        drop(old);
    }
}

#[test]
fn sparse_logical_windows_and_preallocation_beyond_eof_use_bounded_scratch() {
    let root = tempfile::tempdir().unwrap();
    let mut manifest = Manifest::create(root.path(), ID, metadata()).unwrap();
    publish(&mut manifest, 55, 1);
    let offset = 128 * 1024 * 1024 + BLOCK_SIZE as u64;
    let commit = Commit {
        generation: 3,
        durable: 2,
        ..manifest.current()
    };
    direct::write(&manifest.file, &commit.encode(offset).unwrap(), offset).unwrap();
    direct::sync_data(&manifest.file).unwrap();
    drop(manifest);
    let account = budget(16 * BLOCK_SIZE);
    let mut manifest = Manifest::inspect(root.path(), ID, 2, Arc::clone(&account), |_| Ok(()))
        .unwrap()
        .recover()
        .unwrap();
    let end = manifest.end();
    direct::preallocate(&manifest.file, end + BLOCK_SIZE as u64, 2 * 1024 * 1024).unwrap();
    direct::preallocate(&manifest.file, end + 4 * 1024 * 1024, 1024 * 1024).unwrap();
    direct::sync_data(&manifest.file).unwrap();
    let allocated_before = manifest.file.metadata().unwrap().blocks() * 512;
    assert!(allocated_before >= 3 * 1024 * 1024);
    let inside = end + 2 * BLOCK_SIZE as u64;
    assert_eq!(
        direct::next_extent(&manifest.file, inside)
            .unwrap()
            .unwrap()
            .start,
        inside
    );
    let result = manifest.reclaim_pages(Arc::clone(&account)).unwrap();
    assert_eq!(
        (
            result.windows,
            result.retained_pages,
            result.tree_page_reads
        ),
        (2, 3, 2)
    );
    assert!(result.removed_tail_mapping_bytes >= 3 * 1024 * 1024);
    assert_eq!(manifest.file.metadata().unwrap().len(), end);
    assert!(direct::next_extent(&manifest.file, end).unwrap().is_none());
    let allocated_after = manifest.file.metadata().unwrap().blocks() * 512;
    assert!(allocated_before - allocated_after >= 3 * 1024 * 1024);
    assert!(account.usage().peak.bytes <= 3 * BLOCK_SIZE);
    current(&manifest.view().unwrap(), 55);
    eprintln!(
        "manifest sparse/tail check: {}",
        serde_json::json!({
            "gc": result, "inode_allocated_before": allocated_before,
            "inode_allocated_after": allocated_after, "metadata_peak": account.usage().peak.bytes,
            "filesystem_free_space_measured": false,
        })
    );
    let repeated = manifest.reclaim_pages(Arc::clone(&account)).unwrap();
    assert_eq!(repeated.removed_tail_mapping_bytes, 0);
    assert_eq!(
        manifest.file.metadata().unwrap().blocks() * 512,
        allocated_after
    );
    drop(manifest);
    assert_eq!(account.usage().current.bytes, 0);
    let recovered = inspect(root.path(), 2).unwrap().recover().unwrap();
    current(&recovered.view().unwrap(), 55);
}

#[test]
fn failed_snapshot_sweep_requires_recovery_before_new_views_or_clones() {
    let root = tempfile::tempdir().unwrap();
    let (manifest, old) = populated(root.path());
    let key = manifest.view().unwrap().key();
    drop((manifest, old));
    let mut snapshot = Snapshot::inspect(root.path(), key, metadata(), |_| Ok(()))
        .unwrap()
        .recover()
        .unwrap();
    let previous = snapshot.view().unwrap();
    let before = bytes(root.path());
    faults::inject(Fault::Punch);
    assert!(snapshot.reclaim_pages(metadata()).is_err());
    assert!(snapshot.failed());
    assert!(snapshot.view().is_err());
    assert!(snapshot.pinned_roots(metadata()).is_err());
    let clone = tempfile::tempdir().unwrap();
    assert!(
        Manifest::clone_snapshot(
            &snapshot,
            clone.path(),
            Identity {
                image: [9; 16],
                ..ID
            },
            metadata()
        )
        .is_err()
    );
    assert_eq!(fs::read_dir(clone.path()).unwrap().count(), 0);
    assert_eq!(bytes(root.path()), before);
    current(&previous, 33);
    drop((snapshot, previous));
    let mut recovered = Snapshot::inspect(root.path(), key, metadata(), |_| Ok(()))
        .unwrap()
        .recover()
        .unwrap();
    recovered.reclaim_pages(metadata()).unwrap();
    assert_eq!(recovered.key(), key);
    current(&recovered.view().unwrap(), 33);
}

#[test]
fn tail_truncate_and_final_sync_failures_leave_exact_eof_and_recoverable_roots() {
    for (fault, after_calls) in [(Fault::Truncate, 0), (Fault::Sync, 1), (Fault::Map, 2)] {
        let root = tempfile::tempdir().unwrap();
        let mut manifest = Manifest::create(root.path(), ID, metadata()).unwrap();
        let end = manifest.end();
        direct::preallocate(&manifest.file, end, 1024 * 1024).unwrap();
        direct::sync_data(&manifest.file).unwrap();
        let before = bytes(root.path());
        faults::inject_after(fault, after_calls);
        assert!(manifest.reclaim_pages(metadata()).is_err());
        assert!(manifest.failed());
        assert_eq!(bytes(root.path()), before);
        assert_eq!(manifest.file.metadata().unwrap().len(), end);
        drop(manifest);
        let mut recovered = inspect(root.path(), 0).unwrap().recover().unwrap();
        recovered.reclaim_pages(metadata()).unwrap();
        assert_eq!(recovered.end(), end);
        assert!(direct::next_extent(&recovered.file, end).unwrap().is_none());
        assert_eq!(recovered.tree().unwrap().get(0).unwrap(), None);
        assert!(!root.path().join("rejected").exists());
    }
}

#[test]
#[ignore = "requires real reflink; run the packaged XFS fixture"]
fn independent_source_snapshot_and_clone_sweeps_preserve_shared_live_pages() {
    let root = tempfile::tempdir().unwrap();
    let source_path = root.path().join("source");
    let snapshot_path = root.path().join("snapshot");
    let clone_path = root.path().join("clone");
    for path in [&source_path, &snapshot_path, &clone_path] {
        fs::create_dir(path).unwrap();
    }
    let (mut source, old) = populated(&source_path);
    let mut snapshot = Snapshot::create(&old, &snapshot_path, metadata()).unwrap();
    let key = snapshot.key();
    let clone_identity = Identity {
        image: [9; 16],
        ..ID
    };
    let mut clone =
        Manifest::clone_snapshot(&snapshot, &clone_path, clone_identity, metadata()).unwrap();
    publish(&mut clone, 77, 1);
    let before_source = bytes(&source_path);
    let result = snapshot.reclaim_pages(metadata()).unwrap();
    assert_eq!(result.retained_pages, 3);
    assert_eq!(bytes(&source_path), before_source);
    let before_snapshot = bytes(&snapshot_path);
    let result = clone.reclaim_pages(metadata()).unwrap();
    assert_eq!(result.retained_pages, 3);
    assert_eq!(bytes(&snapshot_path), before_snapshot);
    assert_eq!(bytes(&source_path), before_source);
    current(&old, 11);
    current(&source.view().unwrap(), 33);
    current(&snapshot.view().unwrap(), 11);
    current(&clone.view().unwrap(), 77);
    assert_eq!(source.reclaim_pages(metadata()).unwrap().roots, 2);
    drop(old);
    assert_eq!(source.reclaim_pages(metadata()).unwrap().roots, 1);
    assert_eq!(bytes(&snapshot_path), before_snapshot);
    assert_eq!(snapshot.key(), key);
    drop((source, snapshot, clone));
    let source = Manifest::inspect(&source_path, ID, 3, metadata(), |_| Ok(()))
        .unwrap()
        .recover()
        .unwrap();
    let snapshot = Snapshot::inspect(&snapshot_path, key, metadata(), |_| Ok(()))
        .unwrap()
        .recover()
        .unwrap();
    let clone = Manifest::inspect(&clone_path, clone_identity, 1, metadata(), |_| Ok(()))
        .unwrap()
        .recover()
        .unwrap();
    current(&source.view().unwrap(), 33);
    current(&snapshot.view().unwrap(), 11);
    current(&clone.view().unwrap(), 77);
}
