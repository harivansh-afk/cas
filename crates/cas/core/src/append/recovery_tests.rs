use super::*;
use format::{Descriptor, RequestId};
use std::fs;

const IMAGE: u64 = 8 * BLOCK_SIZE as u64;
fn create(path: &Path) -> Log {
    Log::create(
        path,
        Config {
            store: [1; 16],
            image: [2; 16],
            image_bytes: IMAGE,
            segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
        },
        Limits::default(),
    )
    .unwrap()
}
fn mutation(sequence: u64) -> Mutation {
    Mutation {
        id: RequestId {
            serial: sequence * 2,
            attachment: 7,
            queue: 1,
            head: sequence as u16,
        },
        sequence,
        offset: 0,
        length: BLOCK_SIZE as u64,
        kind: Kind::Write,
    }
}
fn write(log: &mut Log, sequence: u64) {
    let m = mutation(sequence);
    let mut builder = Builder::new(IMAGE, BLOCK_SIZE).unwrap();
    builder
        .write(m.id, m.offset, BLOCK_SIZE, |bytes| {
            bytes.fill(sequence as u8);
            Ok(())
        })
        .unwrap();
    let batch = log.append(builder).unwrap();
    let actual: Descriptor = Header::decode(&batch.bytes()[..BLOCK_SIZE], IMAGE)
        .unwrap()
        .descriptors()
        .next()
        .unwrap();
    assert_eq!(Mutation::from(actual), m);
}
fn contents(log: &mut Log) -> Vec<u8> {
    let mut bytes = AlignedBuffer::new(BLOCK_SIZE);
    log.read_into(0, &mut bytes).unwrap();
    bytes.as_slice().to_vec()
}
fn tree(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut entries: Vec<_> = fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().into_string().unwrap(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    entries
}
fn inspect(path: &Path) -> Recovery {
    Log::inspect(path, Limits::default()).unwrap()
}

#[test]
fn inspection_is_read_only_and_owns_actual_candidate_file_locks() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("log");
    let mut log = create(&path);
    write(&mut log, 1);
    drop(log);
    let segment = path.join(segment::name(1));
    let file = fs::OpenOptions::new().write(true).open(&segment).unwrap();
    file.set_len(file.metadata().unwrap().len() + 17).unwrap();
    drop(file);
    let before = tree(&path);
    let recovery = inspect(&path);
    assert_eq!(recovery.status().published, 1);
    assert_eq!(tree(&path), before);
    assert!(Log::inspect(&path, Limits::default()).is_err());
    assert!(direct::open(&segment, false).is_err());
    drop(recovery);
    assert_eq!(tree(&path), before);
    assert!(direct::open(&segment, false).is_ok());
}

#[test]
fn invalid_prefix_epoch_identity_and_missing_ownership_cannot_repair_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("log");
    let mut log = create(&path);
    write(&mut log, 1);
    write(&mut log, 2);
    drop(log);
    let segment = path.join(segment::name(1));
    let file = fs::OpenOptions::new().write(true).open(&segment).unwrap();
    file.set_len(file.metadata().unwrap().len() + 17).unwrap();
    drop(file);
    let before = tree(&path);
    assert!(inspect(&path).live(3, 1, 3, vec![mutation(3)]).is_err());
    assert!(inspect(&path).live(2, 2, 2, vec![]).is_err());
    assert!(inspect(&path).live(2, 1, 1, vec![]).is_err());
    assert!(inspect(&path).live(2, 1, 3, vec![]).is_err());
    assert!(inspect(&path).live(2, 1, 4, vec![mutation(4)]).is_err());
    let mut wrong = mutation(1);
    wrong.offset = BLOCK_SIZE as u64;
    assert!(inspect(&path).live(2, 1, 2, vec![wrong]).is_err());
    let mut wrong = mutation(1);
    wrong.id.serial += 1;
    assert!(inspect(&path).live(2, 1, 2, vec![wrong]).is_err());
    assert!(
        inspect(&path)
            .live(2, 1, 2, vec![mutation(1), mutation(1)])
            .is_err()
    );
    assert!(
        inspect(&path)
            .live(2, 1, 2, vec![mutation(1); 1025])
            .is_err()
    );
    assert_eq!(tree(&path), before);
}

#[test]
fn present_old_write_is_verified_but_cannot_overwrite_a_newer_version() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("log");
    let mut log = create(&path);
    write(&mut log, 1);
    write(&mut log, 2);
    drop(log);
    for _ in 0..6 {
        let mut replay = inspect(&path).live(2, 1, 2, vec![mutation(1)]).unwrap();
        assert!(replay.next().is_none());
        assert!(
            replay
                .replay_next(|_| panic!("old payload must not be gathered"))
                .is_err()
        );
        let mut log = replay.finish().unwrap();
        assert_eq!(contents(&mut log), vec![2; BLOCK_SIZE]);
        assert_eq!((log.status().epoch, log.status().segments), (1, 1));
        assert_eq!((log.status().published, log.status().durable), (2, 2));
    }
}

#[test]
fn interrupted_replay_retains_original_sequences_and_finishes_with_a_new_fence() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("log");
    let mut log = create(&path);
    write(&mut log, 1);
    drop(log);
    let mut replay = inspect(&path)
        .live(1, 1, 3, vec![mutation(2), mutation(3)])
        .unwrap();
    assert_eq!(replay.next(), Some(mutation(2)));
    replay
        .replay_next(|bytes| {
            bytes.fill(2);
            Ok(())
        })
        .unwrap();
    assert_eq!(replay.published(), 2);
    drop(replay); // A second daemon replacement before mutation 3 or sync.
    let mut replay = inspect(&path)
        .live(1, 1, 3, vec![mutation(2), mutation(3)])
        .unwrap();
    assert_eq!(replay.next(), Some(mutation(3)));
    replay
        .replay_next(|bytes| {
            bytes.fill(3);
            Ok(())
        })
        .unwrap();
    let mut log = replay.finish().unwrap();
    assert_eq!(contents(&mut log), vec![3; BLOCK_SIZE]);
    assert_eq!(
        (
            log.status().published,
            log.status().durable,
            log.status().epoch
        ),
        (3, 3, 1)
    );
    drop(log);
    let log = inspect(&path).fresh(3).unwrap();
    assert_eq!(log.status().epoch, 2);
}

#[test]
fn torn_a_before_complete_b_requires_both_original_inflight_owners() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("log");
    let mut log = create(&path);
    write(&mut log, 1);
    write(&mut log, 2);
    drop(log);
    let segment = path.join(segment::name(1));
    use std::os::unix::fs::FileExt;
    let file = fs::OpenOptions::new().write(true).open(&segment).unwrap();
    file.write_all_at(&[0xff], 2 * BLOCK_SIZE as u64).unwrap();
    drop(file);
    let before = tree(&path);
    assert!(inspect(&path).live(0, 1, 2, vec![mutation(2)]).is_err());
    assert_eq!(tree(&path), before);
    let mut replay = inspect(&path)
        .live(0, 1, 2, vec![mutation(1), mutation(2)])
        .unwrap();
    for value in 1..=2 {
        replay
            .replay_next(|bytes| {
                bytes.fill(value);
                Ok(())
            })
            .unwrap();
    }
    let mut log = replay.finish().unwrap();
    assert_eq!(contents(&mut log), vec![2; BLOCK_SIZE]);
    assert_eq!(log.status().rejected_bytes, 4 * BLOCK_SIZE as u64);
    let archives: Vec<_> = fs::read_dir(path.join("rejected")).unwrap().collect();
    assert_eq!(archives.len(), 1);
    let archived = fs::read(archives[0].as_ref().unwrap().path()).unwrap();
    assert_eq!(archived, before[0].1[BLOCK_SIZE..]);
}

#[test]
fn failed_gather_is_retryable_but_unfinished_recovery_cannot_serve() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("log");
    drop(create(&path));
    let mut replay = inspect(&path).live(0, 1, 1, vec![mutation(1)]).unwrap();
    assert!(
        replay
            .replay_next(|bytes| {
                bytes.fill(9);
                Err(io::Error::other("guest memory changed"))
            })
            .is_err()
    );
    assert_eq!((replay.published(), replay.next()), (0, Some(mutation(1))));
    assert!(replay.finish().is_err());
    let mut zero = mutation(1);
    zero.kind = Kind::Zero;
    zero.length = IMAGE;
    let mut replay = inspect(&path).live(0, 1, 1, vec![zero]).unwrap();
    replay
        .replay_next(|_| panic!("ZERO has no payload"))
        .unwrap();
    let mut log = replay.finish().unwrap();
    assert_eq!(contents(&mut log), vec![0; BLOCK_SIZE]);
    assert_eq!(log.status().durable, 1);
}

#[test]
fn failed_recovery_sync_never_returns_a_serving_log() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("log");
    let mut log = create(&path);
    write(&mut log, 1);
    drop(log);
    let replay = inspect(&path).live(1, 1, 1, vec![]).unwrap();
    direct::faults::inject(direct::faults::Fault::Sync);
    assert!(replay.finish().is_err());
    // The surviving FENCE alone is insufficient. The replacement adds and
    // synchronizes another recovery FENCE before it can return a serving log.
    let before = fs::metadata(path.join(segment::name(1))).unwrap().len();
    let replay = inspect(&path).live(1, 1, 1, vec![]).unwrap();
    let mut log = replay.finish().unwrap();
    assert_eq!(contents(&mut log), vec![1; BLOCK_SIZE]);
    assert_eq!(log.status().durable, 1);
    assert_eq!(
        fs::metadata(path.join(segment::name(1))).unwrap().len(),
        before + BLOCK_SIZE as u64
    );
}

#[test]
fn recovery_after_a_full_segment_keeps_the_epoch_and_unique_segment_numbers() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("log");
    let mut log = create(&path);
    for sequence in 1..=255 {
        write(&mut log, sequence);
    }
    log.flush().unwrap();
    assert_eq!(
        fs::metadata(path.join(segment::name(1))).unwrap().len(),
        2 * MAX_REQUEST_BYTES as u64
    );
    drop(log);
    let mut next = mutation(256);
    next.id.head = 0;
    let mut replay = inspect(&path).live(255, 1, 256, vec![next]).unwrap();
    replay
        .replay_next(|bytes| {
            bytes.fill(42);
            Ok(())
        })
        .unwrap();
    let mut log = replay.finish().unwrap();
    assert_eq!(contents(&mut log), vec![42; BLOCK_SIZE]);
    assert_eq!(
        (
            log.status().epoch,
            log.status().segments,
            log.status().durable
        ),
        (1, 2, 256)
    );
    drop(log);
    let log = inspect(&path).fresh(256).unwrap();
    assert_eq!((log.status().epoch, log.status().segments), (3, 3));
}
