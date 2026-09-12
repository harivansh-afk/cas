use super::*;

const CLONE: Identity = Identity {
    image: [3; 16],
    ..ID
};

fn populated(path: &Path) -> Manifest {
    let mut manifest = Manifest::create(path, ID, metadata()).unwrap();
    let changes: Vec<_> = (0..140)
        .map(|block| change(block, (block + 1) as u8))
        .collect();
    let prepared = manifest.prepare(&changes, 7).unwrap();
    manifest.publish(prepared).unwrap();
    manifest
}

fn key(manifest: &Manifest) -> SnapshotKey {
    SnapshotKey {
        commit: manifest.current(),
        end: manifest.end(),
    }
}

fn destination(root: &Path, name: &str) -> std::path::PathBuf {
    let path = root.join(name);
    fs::create_dir(&path).unwrap();
    path
}

fn frozen(path: &Path) -> Snapshot {
    let manifest = populated(path);
    let key = key(&manifest);
    drop(manifest);
    Snapshot::inspect(path, key, metadata(), |_| Ok(()))
        .unwrap()
        .recover()
        .unwrap()
}

fn model(view: &View, changes: &[(usize, Option<Hash>)]) {
    let mut oracle = [None; 512];
    for (block, hash) in oracle[..140].iter_mut().enumerate() {
        *hash = Some([(block + 1) as u8; 32]);
    }
    for &(block, hash) in changes {
        oracle[block] = hash;
    }
    let mut tree = view.tree().unwrap();
    for (block, expected) in oracle.into_iter().enumerate() {
        assert_eq!(tree.get(block as u64).unwrap(), expected, "block {block}");
    }
}

#[test]
fn immutable_inspection_requires_exact_eof_commit_and_chunk_dependencies() {
    let root = tempfile::tempdir().unwrap();
    let manifest = populated(root.path());
    let expected = key(&manifest);
    drop(manifest);
    let original = bytes(root.path());
    for invalid in [
        SnapshotKey {
            end: expected.end - BLOCK_SIZE as u64,
            ..expected
        },
        SnapshotKey {
            commit: Commit {
                generation: expected.commit.generation + 1,
                ..expected.commit
            },
            ..expected
        },
        SnapshotKey {
            commit: Commit {
                durable: expected.commit.durable + 1,
                ..expected.commit
            },
            ..expected
        },
        SnapshotKey {
            commit: Commit {
                image: [9; 16],
                ..expected.commit
            },
            ..expected
        },
        SnapshotKey {
            commit: Commit {
                root: Root::default(),
                ..expected.commit
            },
            ..expected
        },
    ] {
        assert!(Snapshot::inspect(root.path(), invalid, metadata(), |_| Ok(())).is_err());
        assert_unchanged(root.path(), &original);
    }
    assert!(
        Snapshot::inspect(root.path(), expected, metadata(), |_| Err(
            io::Error::other("missing shared chunk")
        ))
        .is_err()
    );
    assert_unchanged(root.path(), &original);
    let inspected = Snapshot::inspect(root.path(), expected, metadata(), |_| Ok(())).unwrap();
    assert_eq!(inspected.selected(), expected);
    assert!(Snapshot::inspect(root.path(), expected, metadata(), |_| Ok(())).is_err());
    faults::inject(Fault::Sync);
    assert!(inspected.recover().is_err());
    assert_unchanged(root.path(), &original);
    let snapshot = Snapshot::inspect(root.path(), expected, metadata(), |_| Ok(()))
        .unwrap()
        .recover()
        .unwrap();
    model(&snapshot.view(), &[]);
    let pin = snapshot.view();
    drop(snapshot);
    assert!(Snapshot::inspect(root.path(), expected, metadata(), |_| Ok(())).is_err());
    drop(pin);
    Snapshot::inspect(root.path(), expected, metadata(), |_| Ok(())).unwrap();
}

#[test]
fn immutable_inspection_does_not_repair_a_suffix_or_corrupt_tree() {
    let root = tempfile::tempdir().unwrap();
    let manifest = populated(root.path());
    let expected = key(&manifest);
    drop(manifest);
    let original = bytes(root.path());
    for corrupt_tree in [false, true] {
        let mut damaged = original.clone();
        if corrupt_tree {
            damaged[expected.commit.root.offset as usize] ^= 1;
        } else {
            damaged.extend_from_slice(&[0; BLOCK_SIZE]);
        }
        fs::write(root.path().join(NAME), &damaged).unwrap();
        assert!(Snapshot::inspect(root.path(), expected, metadata(), |_| Ok(())).is_err());
        assert_unchanged(root.path(), &damaged);
    }
}

#[test]
fn metadata_identity_and_reflink_denials_never_fall_back_to_a_copy() {
    let root = tempfile::tempdir().unwrap();
    let source_path = destination(root.path(), "source");
    let source = frozen(&source_path);
    let before = bytes(&source_path);
    let denied = destination(root.path(), "metadata-denied");
    assert!(Snapshot::create(&source.view(), &denied, budget(BLOCK_SIZE - 1)).is_err());
    assert!(!denied.join(NAME).exists());
    assert!(Manifest::clone_snapshot(&source, &denied, CLONE, budget(BLOCK_SIZE - 1)).is_err());
    assert!(!denied.join(NAME).exists());
    for identity in [
        ID,
        Identity {
            store: [8; 16],
            ..CLONE
        },
        Identity {
            image_bytes: ID.image_bytes / 2,
            ..CLONE
        },
    ] {
        assert!(Manifest::clone_snapshot(&source, &denied, identity, metadata()).is_err());
        assert!(!denied.join(NAME).exists());
    }
    for clone in [false, true] {
        let denied = destination(
            root.path(),
            if clone {
                "clone-unsupported"
            } else {
                "snapshot-unsupported"
            },
        );
        faults::inject(Fault::Reflink);
        let error = if clone {
            Manifest::clone_snapshot(&source, &denied, CLONE, metadata()).err()
        } else {
            Snapshot::create(&source.view(), &denied, metadata()).err()
        }
        .unwrap();
        assert_eq!(error.raw_os_error(), Some(libc::EOPNOTSUPP));
        assert!(bytes(&denied).is_empty());
        assert_unchanged(&source_path, &before);
    }
}

#[test]
#[ignore = "requires real reflink; run the packaged XFS fixture"]
fn reflink_snapshot_captures_exact_old_root_and_clone_gets_a_private_namespace() {
    let root = tempfile::tempdir().unwrap();
    let source_path = destination(root.path(), "source");
    let snapshot_path = destination(root.path(), "snapshot");
    let images = destination(root.path(), "images");
    let clone_path = destination(&images, "03030303030303030303030303030303");
    let mut source = populated(&source_path);
    let captured = source.view().unwrap();
    let original = bytes(&source_path);
    let prepared = source.prepare(&[change(1, 199)], 8).unwrap();
    source.publish(prepared).unwrap();
    assert!(source.current().durable > captured.commit().durable);
    assert_ne!(source.current().root, captured.commit().root);
    let snapshot = Snapshot::create(&captured, &snapshot_path, metadata()).unwrap();
    assert_eq!(
        snapshot.key(),
        SnapshotKey {
            commit: captured.commit(),
            end: captured.end()
        }
    );
    assert_eq!(bytes(&snapshot_path), original);
    model(&snapshot.view(), &[]);
    model(&source.view().unwrap(), &[(1, Some([199; 32]))]);
    let mut clone = Manifest::clone_snapshot(&snapshot, &clone_path, CLONE, metadata()).unwrap();
    assert_eq!(clone.current().image, CLONE.image);
    assert_eq!(
        (clone.current().generation, clone.current().durable),
        (1, 0)
    );
    assert_eq!(clone.current().root, snapshot.key().commit.root);
    model(&clone.view().unwrap(), &[]);
    {
        use crate::{append, segments::Tickets};
        let log = append::Log::create_shared(
            Tickets::open(root.path(), metadata()).unwrap(),
            append::Config {
                store: CLONE.store,
                image: CLONE.image,
                image_bytes: CLONE.image_bytes,
                segment_bytes: 2 * crate::MAX_REQUEST_BYTES as u64,
            },
            append::Limits::default(),
            metadata(),
            clone.view().unwrap(),
        )
        .unwrap();
        let status = log.status();
        assert_eq!(
            (
                status.published,
                status.issued,
                status.durable,
                status.compacted
            ),
            (0, 0, 0, 0)
        );
        let plan = log.read_plan(0, BLOCK_SIZE, 0).unwrap();
        assert!(!plan.staged(0));
        assert!(plan.manifest().unwrap().same(&clone.view().unwrap()));
    }
    let prepared = clone
        .prepare(
            &[
                change(3, 201),
                Extent {
                    start: 20,
                    end: 25,
                    hash: None,
                },
            ],
            1,
        )
        .unwrap();
    clone.publish(prepared).unwrap();
    let edits = [
        (3, Some([201; 32])),
        (20, None),
        (21, None),
        (22, None),
        (23, None),
        (24, None),
    ];
    model(&clone.view().unwrap(), &edits);
    model(&snapshot.view(), &[]);
    model(&source.view().unwrap(), &[(1, Some([199; 32]))]);
    assert_eq!(bytes(&snapshot_path), original);
    drop(clone);
    let clone = Manifest::inspect(&clone_path, CLONE, 1, metadata(), |_| Ok(()))
        .unwrap()
        .recover()
        .unwrap();
    model(&clone.view().unwrap(), &edits);
    let key = snapshot.key();
    drop(snapshot);
    let recovered = Snapshot::inspect(&snapshot_path, key, metadata(), |_| Ok(()))
        .unwrap()
        .recover()
        .unwrap();
    model(&recovered.view(), &[]);
}

#[test]
#[ignore = "requires real reflink; run the packaged XFS fixture"]
fn failed_snapshot_verification_or_sync_leaves_the_source_and_orphan_intact() {
    for fault in [Fault::Read, Fault::FileSync, Fault::DirectorySync] {
        let root = tempfile::tempdir().unwrap();
        let source_path = destination(root.path(), "source");
        let target = destination(root.path(), "failed");
        let source = populated(&source_path);
        let before = bytes(&source_path);
        faults::inject(fault);
        assert!(Snapshot::create(&source.view().unwrap(), &target, metadata()).is_err());
        assert_unchanged(&source_path, &before);
        assert_eq!(bytes(&target), before);
        // The constructor returned no snapshot; only explicit inspection can
        // stabilize this complete orphan. It has not entered any catalog.
        let inspected = Snapshot::inspect(&target, key(&source), metadata(), |_| Ok(())).unwrap();
        model(&inspected.recover().unwrap().view(), &[]);
    }
}

#[test]
#[ignore = "requires real reflink; run the packaged XFS fixture"]
fn interrupted_clone_publication_never_changes_the_snapshot() {
    for fault in [
        Fault::Allocate,
        Fault::Write,
        Fault::FileSync,
        Fault::DirectorySync,
    ] {
        let root = tempfile::tempdir().unwrap();
        let source_path = destination(root.path(), "source");
        let target = destination(root.path(), "failed-clone");
        let source = frozen(&source_path);
        let before = bytes(&source_path);
        faults::inject(fault);
        assert!(Manifest::clone_snapshot(&source, &target, CLONE, metadata()).is_err());
        assert_unchanged(&source_path, &before);
        let failed = bytes(&target);
        if matches!(fault, Fault::Allocate | Fault::Write) {
            assert!(Manifest::inspect(&target, CLONE, 0, metadata(), |_| Ok(())).is_err());
            assert_unchanged(&target, &failed);
        } else {
            let inspected = Manifest::inspect(&target, CLONE, 0, metadata(), |_| Ok(())).unwrap();
            assert_eq!(
                (
                    inspected.selected().commit.generation,
                    inspected.selected().commit.durable
                ),
                (1, 0)
            );
            model(&inspected.recover().unwrap().view().unwrap(), &[]);
        }
    }
}
