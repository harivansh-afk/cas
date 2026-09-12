use super::*;
use std::{sync::mpsc, time::Duration};

#[test]
fn missing_roots_and_abandoned_mark_or_sweep_fail_all_readers() {
    for phase in 0..4 {
        let root = tempfile::tempdir().unwrap();
        let mut store = create(root.path());
        insert(&mut store, &[[1; BLOCK_SIZE], [2; BLOCK_SIZE]]).unwrap();
        let reader = store.reader().unwrap();
        let mut collection = store.begin_collection().unwrap();
        if phase == 0 {
            assert!(collection.mark(&hash(&[99; BLOCK_SIZE])).is_err());
            assert!(reader.status().failed); // Failure is visible before owner drop.
            drop(collection);
        } else if phase == 1 {
            drop(collection);
        } else {
            collection.mark(&hash(&[1; BLOCK_SIZE])).unwrap();
            let sweep = collection.finish_marking().unwrap();
            if phase == 2 {
                drop(sweep);
            } else {
                assert!(sweep.finish().is_err());
            }
        }
        assert!(reader.status().failed);
        assert!(reader.plan(hash(&[1; BLOCK_SIZE])).is_err());
        drop((reader, store));
        let store = inspect(root.path()).unwrap().recover().unwrap();
        read(&store, &[1; BLOCK_SIZE]);
        read(&store, &[2; BLOCK_SIZE]);
    }
}

#[test]
fn copying_faults_retain_live_data_and_recovery_cleans_durable_duplicates() {
    let cases = [
        (Fault::Read, 0),
        (Fault::Read, 1),
        (Fault::Read, 2),
        (Fault::Allocate, 0),
        (Fault::Allocate, 1),
        (Fault::Write, 0),
        (Fault::Write, 1),
        (Fault::ShortWrite, 1),
        (Fault::FileSync, 0),
        (Fault::DirectorySync, 0),
        (Fault::DirectorySync, 1),
        (Fault::Sync, 0),
        (Fault::Sync, 1),
        (Fault::Map, 0),
        (Fault::Map, 1),
        (Fault::Truncate, 0),
        (Fault::Unlink, 0),
    ];
    for (index, (fault, after)) in cases.into_iter().enumerate() {
        let root = tempfile::tempdir().unwrap();
        let mut store = create(root.path());
        insert(
            &mut store,
            &[[1; BLOCK_SIZE], [2; BLOCK_SIZE], [3; BLOCK_SIZE]],
        )
        .unwrap();
        let reader = store.reader().unwrap();
        let mut collection = store.begin_collection().unwrap();
        collection.mark(&hash(&[1; BLOCK_SIZE])).unwrap();
        collection.mark(&hash(&[3; BLOCK_SIZE])).unwrap();
        let mut sweep = collection.finish_marking().unwrap();
        faults::inject_after(fault, after);
        assert!(sweep.clean_next().is_err(), "fault case {index}");
        assert!(reader.status().failed, "fault case {index}");
        assert!(reader.plan(hash(&[1; BLOCK_SIZE])).is_err());
        drop(sweep);
        drop((reader, store));
        let mut store = inspect(root.path()).unwrap().recover().unwrap();
        read(&store, &[1; BLOCK_SIZE]);
        read(&store, &[3; BLOCK_SIZE]);
        collect(&mut store, &[[1; BLOCK_SIZE], [3; BLOCK_SIZE]]);
        assert_tail_free(&store);
        assert_eq!(store.status().chunks, 2);
        drop(store);
        let store = inspect(root.path()).unwrap().recover().unwrap();
        assert_eq!(store.status().chunks, 2);
        read(&store, &[1; BLOCK_SIZE]);
        read(&store, &[3; BLOCK_SIZE]);
    }
}

#[test]
fn read_admission_stays_closed_across_copy_sync_unlink_and_unwind() {
    for boundary in [Fault::Sync, Fault::Unlink] {
        for unwind in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let mut store = create(root.path());
            insert(&mut store, &[[1; BLOCK_SIZE], [2; BLOCK_SIZE]]).unwrap();
            let reader = store.reader().unwrap();
            let (entered, observed) = mpsc::channel();
            let (release, resume) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                faults::pause_before(boundary, entered, resume);
                collect(&mut store, &[[1; BLOCK_SIZE]]);
                store
            });
            observed.recv_timeout(Duration::from_secs(3)).unwrap();
            assert!(reader.status().collecting && !reader.status().failed);
            assert!(
                matches!(reader.plan(hash(&[1; BLOCK_SIZE])), Err(e) if e.kind() == io::ErrorKind::WouldBlock)
            );
            if unwind {
                drop(release);
            } else {
                release.send(()).unwrap();
            }
            let result = worker.join();
            assert_eq!(result.is_err(), unwind);
            assert_eq!(reader.status().failed, unwind);
            if let Ok(store) = result {
                read(&store, &[1; BLOCK_SIZE]);
            }
            drop(reader);
            let store = inspect(root.path()).unwrap().recover().unwrap();
            read(&store, &[1; BLOCK_SIZE]);
        }
    }
}

#[test]
fn corrupt_victims_fail_before_deletion_or_live_hash_relocation() {
    use crate::encoding::{checksum, put32};
    for corruption in 0..5 {
        let root = tempfile::tempdir().unwrap();
        let mut store = create(root.path());
        insert(&mut store, &[[1; BLOCK_SIZE], [2; BLOCK_SIZE]]).unwrap();
        let path = root.path().join("chunks").join(segments::name(1));
        let mut bytes = fs::read(&path).unwrap();
        match corruption {
            0 => bytes.extend_from_slice(&[0; BLOCK_SIZE]),
            1 => bytes[0] ^= 1,
            2 => bytes[BLOCK_SIZE] ^= 1,
            3 => bytes[2 * BLOCK_SIZE] ^= 1,
            _ => {
                bytes[2 * BLOCK_SIZE] ^= 1;
                let crc = crc32fast::hash(&bytes[2 * BLOCK_SIZE..3 * BLOCK_SIZE]);
                let header = &mut bytes[BLOCK_SIZE..2 * BLOCK_SIZE];
                put32(header, 104, crc);
                put32(header, 56, checksum(header, 56));
            }
        }
        fs::write(&path, &bytes).unwrap();
        let mut collection = store.begin_collection().unwrap();
        collection.mark(&hash(&[1; BLOCK_SIZE])).unwrap();
        let mut sweep = collection.finish_marking().unwrap();
        assert!(sweep.clean_next().is_err());
        drop(sweep);
        assert!(store.status().failed);
        assert_eq!(store.shared.tickets.status().highest, 1);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(!path.parent().unwrap().join("rejected").exists());
    }
}

#[test]
fn dead_header_truncation_failures_preserve_the_greatest_ticket() {
    for (fault, after) in [
        (Fault::Map, 0),
        (Fault::Truncate, 0),
        (Fault::Sync, 0),
        (Fault::Map, 1),
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut store = create(root.path());
        insert(&mut store, &[[1; BLOCK_SIZE]]).unwrap();
        let path = root.path().join("chunks").join(segments::name(1));
        let original_header = fs::read(&path).unwrap()[..BLOCK_SIZE].to_vec();
        let mut sweep = store.begin_collection().unwrap().finish_marking().unwrap();
        faults::inject_after(fault, after);
        assert!(sweep.clean_next().is_err());
        drop(sweep);
        assert!(store.status().failed);
        assert_eq!(&fs::read(&path).unwrap()[..BLOCK_SIZE], original_header);
        drop(store);
        let mut store = inspect(root.path()).unwrap().recover().unwrap();
        assert_eq!(store.shared.tickets.status().highest, 1);
        collect(&mut store, &[]);
        assert_tail_free(&store);
        insert(&mut store, &[[2; BLOCK_SIZE]]).unwrap();
        assert_eq!(store.shared.tickets.status().highest, 2);
        read(&store, &[2; BLOCK_SIZE]);
    }
}
