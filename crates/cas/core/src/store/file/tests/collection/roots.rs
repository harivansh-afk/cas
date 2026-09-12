use super::*;

fn publish_block(manifest: &mut Manifest, block: u8, durable: u64) {
    let change = Extent {
        start: 0,
        end: 1,
        hash: Some(hash(&[block; BLOCK_SIZE])),
    };
    let prepared = manifest.prepare(&[change], durable).unwrap();
    manifest.publish(prepared).unwrap();
}

fn mark_roots(collection: &mut Collection<'_>, roots: &crate::manifest::file::Roots<'_>) {
    roots
        .walk(|extent| match extent.hash {
            Some(hash) => collection.mark(&hash),
            None => Ok(()),
        })
        .unwrap();
}

fn finish_collection(collection: Collection<'_>) {
    let mut sweep = collection.finish_marking().unwrap();
    while sweep.clean_next().unwrap().is_some() {}
    sweep.finish().unwrap();
}

#[test]
fn image_and_old_view_roots_keep_shared_chunks_until_the_last_reference_changes() {
    let root = tempfile::tempdir().unwrap();
    let mut store = create(root.path());
    insert(&mut store, &blocks(1, 4)).unwrap();
    let first_path = image_path(root.path());
    let second_path = root.path().join("images/03030303030303030303030303030303");
    fs::create_dir(&second_path).unwrap();
    let mut first = Manifest::create(&first_path, IMAGE, metadata()).unwrap();
    let mut second = Manifest::create(
        &second_path,
        Identity {
            image: [3; 16],
            ..IMAGE
        },
        metadata(),
    )
    .unwrap();
    publish_block(&mut first, 1, 1);
    let old = first.view().unwrap();
    publish_block(&mut first, 2, 2);
    publish_block(&mut second, 3, 1);
    let mut collection = store.begin_collection().unwrap();
    mark_roots(&mut collection, &first.pinned_roots(metadata()).unwrap());
    mark_roots(&mut collection, &second.pinned_roots(metadata()).unwrap());
    finish_collection(collection);
    assert_eq!(store.status().chunks, 3);
    assert_eq!(
        old.tree().unwrap().get(0).unwrap(),
        Some(hash(&[1; BLOCK_SIZE]))
    );
    for block in blocks(1, 3) {
        read(&store, &block);
    }
    drop(old);
    publish_block(&mut second, 2, 2);
    let mut collection = store.begin_collection().unwrap();
    mark_roots(&mut collection, &first.pinned_roots(metadata()).unwrap());
    mark_roots(&mut collection, &second.pinned_roots(metadata()).unwrap());
    finish_collection(collection);
    assert_eq!(store.status().chunks, 1);
    read(&store, &[2; BLOCK_SIZE]);
    assert!(store.plan(hash(&[1; BLOCK_SIZE])).unwrap().is_none());
    assert!(store.plan(hash(&[3; BLOCK_SIZE])).unwrap().is_none());
    drop((first, second, store));
    let inspected = inspect(root.path()).unwrap();
    for (path, image) in [
        (&first_path, IMAGE),
        (
            &second_path,
            Identity {
                image: [3; 16],
                ..IMAGE
            },
        ),
    ] {
        let manifest = Manifest::inspect(path, image, 2, metadata(), |hash| {
            require(inspected.contains(&hash), "missing marked chunk")
        })
        .unwrap()
        .recover()
        .unwrap();
        assert_eq!(
            manifest.tree().unwrap().get(0).unwrap(),
            Some(hash(&[2; BLOCK_SIZE]))
        );
    }
    read(&inspected.recover().unwrap(), &[2; BLOCK_SIZE]);
}

#[test]
#[ignore = "requires an XFS reflink fixture"]
fn reflink_snapshot_and_private_clone_roots_survive_shared_chunk_relocation() {
    use crate::manifest::file::Snapshot;
    let root = tempfile::tempdir().unwrap();
    let mut store = create(root.path());
    insert(&mut store, &blocks(1, 4)).unwrap();
    let source_path = image_path(root.path());
    let snapshot_path = root.path().join("snapshot");
    let clone_path = root.path().join("images/03030303030303030303030303030303");
    fs::create_dir(&snapshot_path).unwrap();
    fs::create_dir(&clone_path).unwrap();
    let mut source = Manifest::create(&source_path, IMAGE, metadata()).unwrap();
    publish_block(&mut source, 1, 1);
    let mut snapshot =
        Snapshot::create(&source.view().unwrap(), &snapshot_path, metadata()).unwrap();
    let mut clone = Manifest::clone_snapshot(
        &snapshot,
        &clone_path,
        Identity {
            image: [3; 16],
            ..IMAGE
        },
        metadata(),
    )
    .unwrap();
    publish_block(&mut source, 2, 2);
    publish_block(&mut clone, 3, 1);
    let mut collection = store.begin_collection().unwrap();
    mark_roots(&mut collection, &source.pinned_roots(metadata()).unwrap());
    mark_roots(&mut collection, &snapshot.pinned_roots(metadata()).unwrap());
    mark_roots(&mut collection, &clone.pinned_roots(metadata()).unwrap());
    finish_collection(collection);
    assert_eq!(store.status().chunks, 3);
    for (view, value) in [
        (source.view().unwrap(), 2),
        (snapshot.view().unwrap(), 1),
        (clone.view().unwrap(), 3),
    ] {
        assert_eq!(
            view.tree().unwrap().get(0).unwrap(),
            Some(hash(&[value; BLOCK_SIZE]))
        );
        read(&store, &[value; BLOCK_SIZE]);
    }
    let key = snapshot.key();
    drop((source, snapshot, clone, store));
    let inspected = inspect(root.path()).unwrap();
    let dependency =
        |hash: Hash| require(inspected.contains(&hash), "missing shared snapshot chunk");
    let source = Manifest::inspect(&source_path, IMAGE, 2, metadata(), dependency).unwrap();
    let snapshot = Snapshot::inspect(&snapshot_path, key, metadata(), dependency).unwrap();
    let clone = Manifest::inspect(
        &clone_path,
        Identity {
            image: [3; 16],
            ..IMAGE
        },
        1,
        metadata(),
        dependency,
    )
    .unwrap();
    let store = inspected.recover().unwrap();
    for (view, value) in [
        (source.recover().unwrap().view().unwrap(), 2),
        (snapshot.recover().unwrap().view().unwrap(), 1),
        (clone.recover().unwrap().view().unwrap(), 3),
    ] {
        assert_eq!(
            view.tree().unwrap().get(0).unwrap(),
            Some(hash(&[value; BLOCK_SIZE]))
        );
        read(&store, &[value; BLOCK_SIZE]);
    }
}
