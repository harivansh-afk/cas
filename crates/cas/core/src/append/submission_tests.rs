use super::*;
use format::RequestId;

fn config() -> Config {
    Config {
        store: [1; 16],
        image: [2; 16],
        image_bytes: 8 * MAX_REQUEST_BYTES as u64,
        segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
    }
}

fn builder(serial: u64, offset: u64, value: u8) -> Builder {
    let mut builder = Builder::new(config().image_bytes, BLOCK_SIZE).unwrap();
    builder
        .write(
            RequestId {
                serial,
                attachment: 1,
                queue: 0,
                head: serial as u16,
            },
            offset,
            BLOCK_SIZE,
            |bytes| {
                bytes.fill(value);
                Ok(())
            },
        )
        .unwrap();
    builder
}

fn write(submission: &Submission) {
    direct::write_bytes(
        submission.file(),
        submission.batch().bytes(),
        submission.offset(),
    )
    .unwrap();
}

fn read(log: &mut Log) -> Vec<u8> {
    let mut buffer = AlignedBuffer::new(BLOCK_SIZE);
    log.read_into(0, &mut buffer).unwrap();
    buffer.as_slice().to_vec()
}

#[test]
fn later_physical_completion_cannot_publish_over_a_hole() {
    let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let path = directory.path().join("log");
    let mut log = Log::create(&path, config(), Limits::default()).unwrap();
    let a = log.prepare_append(builder(1, 0, 0x11)).unwrap();
    let b = log.prepare_append(builder(2, 0, 0x22)).unwrap();
    assert_eq!((log.status().issued, log.status().published), (2, 0));
    assert!(a.offset() < b.offset());
    write(&b);
    assert!(matches!(log.publish_append(&b), Err(Error::Pending)));
    assert_eq!(read(&mut log), vec![0; BLOCK_SIZE]);
    // The max-completed negative control would claim prefix 2 here.
    let wrong_prefix = b.batch().envelope().last;
    assert_eq!(wrong_prefix, 2);
    assert_ne!(wrong_prefix, log.status().published);
    write(&a);
    log.publish_append(&a).unwrap();
    assert_eq!(read(&mut log), vec![0x11; BLOCK_SIZE]);
    log.publish_append(&b).unwrap();
    assert_eq!(read(&mut log), vec![0x22; BLOCK_SIZE]);
    assert!(log.publish_append(&b).is_err());
    log.flush().unwrap();
    drop((a, b, log));
    assert_eq!(
        read(&mut Log::open(&path, Limits::default()).unwrap()),
        vec![0x22; BLOCK_SIZE]
    );
}

#[test]
fn a_fence_freezes_submission_until_covered_io_and_sync_finish() {
    let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let mut log = Log::create(directory.path().join("log"), config(), Limits::default()).unwrap();
    let a = log.prepare_append(builder(1, 0, 0x11)).unwrap();
    let fence = log.prepare_fence().unwrap();
    let later = builder(2, 0, 0x22);
    assert!(matches!(log.check_append(&later), Err(Error::Pending)));
    assert!(matches!(log.prepare_fence(), Err(Error::Pending)));
    assert!(matches!(log.rollover(), Err(Error::Pending)));
    write(&fence); // even a completed FENCE cannot cover unfinished data
    assert!(!log.ready_to_sync(&fence).unwrap());
    assert!(matches!(log.complete_sync(&fence), Err(Error::Pending)));
    assert_eq!(log.status().durable, 0);
    write(&a);
    log.publish_append(&a).unwrap();
    assert!(log.ready_to_sync(&fence).unwrap());
    assert_eq!(read(&mut log), vec![0x11; BLOCK_SIZE]); // reads continue
    assert!(matches!(log.check_append(&later), Err(Error::Pending)));
    direct::sync_data(fence.file()).unwrap();
    assert_eq!(log.complete_sync(&fence).unwrap(), 1);
    assert!(log.complete_sync(&fence).is_err());
    let b = log.prepare_append(later).unwrap();
    assert!(b.offset() > fence.offset());
    assert_eq!((log.status().durable, log.status().issued), (1, 2));
    write(&b);
    log.publish_append(&b).unwrap();
    log.flush().unwrap();
}

#[test]
fn failure_stops_older_successes_and_never_advances_durability() {
    let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let mut log = Log::create(directory.path().join("log"), config(), Limits::default()).unwrap();
    let a = log.prepare_append(builder(1, 0, 0x11)).unwrap();
    let fence = log.prepare_fence().unwrap();
    write(&a);
    log.fail(); // later IO reports an error before A's CQE is processed
    assert!(matches!(log.publish_append(&a), Err(Error::Failed)));
    assert!(matches!(log.complete_sync(&fence), Err(Error::Failed)));
    assert!(matches!(
        log.check_append(&builder(2, 0, 0x22)),
        Err(Error::Failed)
    ));
    assert_eq!((log.status().published, log.status().durable), (0, 0));
}

#[test]
fn dropped_writer_does_not_release_outstanding_io_file_description() {
    let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let path = directory.path().join("log");
    let mut log = Log::create(&path, config(), Limits::default()).unwrap();
    let pending = log.prepare_append(builder(1, 0, 0x11)).unwrap();
    let fd_path = path.join(segment::name(1));
    drop(log);
    assert!(direct::open(&fd_path, false).is_err());
    write(&pending);
    drop(pending);
    let recovered = Log::open(path, Limits::default()).unwrap();
    assert_eq!(recovered.status().published, 1);
}

#[test]
fn index_growth_is_reserved_for_all_unpublished_descriptors() {
    let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let mut log = Log::create(
        directory.path().join("log"),
        config(),
        Limits {
            intervals: 126,
            ..Limits::default()
        },
    )
    .unwrap();
    let mut pending = Vec::new();
    for serial in 1..=63 {
        pending.push(
            log.prepare_append(builder(serial, (serial - 1) * BLOCK_SIZE as u64, 0x11))
                .unwrap(),
        );
    }
    let blocked = builder(64, 63 * BLOCK_SIZE as u64, 0x22);
    assert!(matches!(log.check_append(&blocked), Err(Error::Capacity)));
    for submission in pending {
        write(&submission);
        log.publish_append(&submission).unwrap();
    }
    // Actual growth is smaller than the reserved worst case; unused metadata
    // capacity becomes available after publication.
    assert!(log.check_append(&blocked).is_ok());
}

#[test]
fn foreign_submission_cannot_publish_into_another_log() {
    let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let mut first =
        Log::create(directory.path().join("first"), config(), Limits::default()).unwrap();
    let mut other =
        Log::create(directory.path().join("other"), config(), Limits::default()).unwrap();
    let append = first.prepare_append(builder(1, 0, 0x11)).unwrap();
    write(&append);
    assert!(other.publish_append(&append).is_err());
    assert_eq!(other.status().published, 0);
    first.publish_append(&append).unwrap();
}

#[test]
fn captured_reads_wait_for_publication_and_pin_the_selected_version() {
    let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let path = directory.path().join("log");
    let mut log = Log::create(&path, config(), Limits::default()).unwrap();
    let a = log.prepare_append(builder(1, 0, 0x11)).unwrap();
    assert!(matches!(
        log.read_plan(0, BLOCK_SIZE, 1),
        Err(Error::Pending)
    ));
    write(&a);
    log.publish_append(&a).unwrap();
    let captured = log.read_plan(0, BLOCK_SIZE, 1).unwrap();
    assert_eq!(captured.ranges().len(), 1);
    assert!(captured.ranges()[0].direct_to_response());
    log.append(builder(2, 0, 0x22)).unwrap();
    assert_eq!(read(&mut log), vec![0x22; BLOCK_SIZE]);
    drop((a, log));
    assert!(direct::open(&path.join(segment::name(1)), false).is_err());
    let mut response = AlignedBuffer::new(BLOCK_SIZE);
    captured.read_into(&mut response).unwrap();
    assert_eq!(response.as_slice(), &[0x11; BLOCK_SIZE]);
    drop(captured);
    assert!(direct::open(&path.join(segment::name(1)), false).is_ok());
}

#[test]
fn partial_read_plan_verifies_the_complete_original_payload() {
    use std::os::unix::fs::FileExt;
    let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let path = directory.path().join("log");
    let mut log = Log::create(&path, config(), Limits::default()).unwrap();
    let mut large = Builder::new(config().image_bytes, 2 * BLOCK_SIZE).unwrap();
    large
        .write(
            RequestId {
                serial: 1,
                attachment: 1,
                queue: 0,
                head: 0,
            },
            0,
            2 * BLOCK_SIZE,
            |bytes| {
                bytes.fill(0x11);
                Ok(())
            },
        )
        .unwrap();
    log.append(large).unwrap();
    let plan = log.read_plan(BLOCK_SIZE as u64, BLOCK_SIZE, 1).unwrap();
    let range = &plan.ranges()[0];
    assert_eq!(range.input_bytes(), 2 * BLOCK_SIZE);
    assert_eq!(range.source(), BLOCK_SIZE..2 * BLOCK_SIZE);
    assert!(!range.direct_to_response());
    let mut response = AlignedBuffer::new(BLOCK_SIZE);
    plan.read_into(&mut response).unwrap();
    assert_eq!(response.as_slice(), &[0x11; BLOCK_SIZE]);
    // Corrupt the unrequested first half. A range-only read would miss this.
    let file = fs::OpenOptions::new()
        .write(true)
        .open(path.join(segment::name(1)))
        .unwrap();
    file.write_all_at(&[0xff], range.offset()).unwrap();
    assert!(plan.read_into(&mut response).is_err());
}
