use super::*;

fn allocation_case(timeout: bool) {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    let old = vec![5; IMAGE_BYTES as usize];
    // Seven whole-image batches leave too little room for another 256 KiB.
    for id in 0..7 {
        write(&mut first, id, 0, &old);
    }
    write(&mut second, 0, 0, &old);
    drained(&mut first, 7);
    drained(&mut second, 1);
    let epoch = first.status.epoch;
    let old_read = Io {
        id: 7,
        operation: Operation::Read {
            offset: 0,
            buffer: AlignedBuffer::new(old.len()),
        },
        permit: first.prepare(Kind::Read(old.len())).unwrap().unwrap(),
        boundary: first.admitted,
    };
    let (entered, observed) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    host.shared.control.lock().unwrap().rotation = Some(Pause { entered, resume });
    assert!(first.prepare(Kind::Write(old.len())).unwrap().is_none());
    assert_eq!(first.admitted, 7);
    observed.recv_timeout(Duration::from_secs(3)).unwrap();
    let promise = host.shared.physical.as_ref().map(|physical| {
        let promised = physical.status().promised;
        assert!(promised >= 2 * MAX_REQUEST_BYTES as u64 + capacity::METADATA_MARGIN);
        promised
    });
    // The worker can acknowledge its pause before the reactor publishes status.
    let deadline = Instant::now() + Duration::from_secs(3);
    while first.report()["status"]["rotating"] != true {
        assert!(
            Instant::now() < deadline,
            "rotation status was not published"
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(first.report()["status"]["published"], 7);
    // Deliver a previously accepted read while the next write waits BEFORE
    // mutation admission. Its captured boundary remains P=7.
    first.send(Command::Io(old_read)).unwrap();
    let response = completed(&mut first);
    assert_eq!(response.id, 7);
    let CompletionData::Read(bytes) = &response.data else {
        panic!("old read completion");
    };
    assert_eq!(bytes.as_slice(), old);
    drop(response);
    let permit = first.prepare(Kind::Control).unwrap().unwrap();
    first.enqueue(8, Operation::Flush, permit).unwrap();
    assert_eq!(completed(&mut first).id, 8);
    assert_eq!(first.admitted, 7);
    read(&mut second, 1, &old);
    write(&mut second, 2, 0, &[7; BLOCK_SIZE]);
    let permit = second.prepare(Kind::Control).unwrap().unwrap();
    second.enqueue(3, Operation::Flush, permit).unwrap();
    completed(&mut second);
    assert_eq!(second.status.durable, 2);
    assert!(Tickets::open(root.path(), Arc::clone(&resources.metadata)).is_err());
    if timeout {
        let deadline = Instant::now() + IO_DEADLINE + Duration::from_secs(3);
        while first.shared.health.lock().unwrap().failure.is_none() {
            assert!(
                Instant::now() < deadline,
                "allocation deadline did not fail the host"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(second.shared.health.lock().unwrap().failure.is_some());
        assert_eq!(first.report()["status"]["published"], 7);
        assert_eq!(
            host.shared
                .physical
                .as_ref()
                .map(|physical| physical.status().promised),
            promise
        );
        assert!(Tickets::open(root.path(), Arc::clone(&resources.metadata)).is_err());
    }
    release.send(()).unwrap();
    if !timeout {
        write(&mut first, 9, 0, &vec![9; old.len()]);
        assert_eq!(first.status.epoch, epoch);
        drained(&mut first, 8);
        drained(&mut second, 2);
        read(&mut first, 10, &vec![9; old.len()]);
    } else {
        assert_eq!(first.admitted, 7); // No owned write or replay obligation was created.
    }
    drop((first, second));
    shutdown(host);
    assert_eq!(resources.metadata.usage().current.bytes, 0);
    let mut host = reopen(root.path(), 2, resources);
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    read(&mut first, 0, &vec![if timeout { 5 } else { 9 }; old.len()]);
    let mut other = old;
    other[..BLOCK_SIZE].fill(7);
    read(&mut second, 0, &other);
    drop((first, second));
    shutdown(host);
}

#[test]
fn allocation_worker_keeps_captured_reads_and_other_image_flushes_live() {
    allocation_case(false);
}

#[test]
fn allocation_worker_deadline_retains_owners_and_prevents_late_installation() {
    allocation_case(true);
}

#[test]
fn unused_admission_token_prevents_rotation_until_its_actual_release() {
    let root = tempfile::tempdir().unwrap();
    let mut host = create(root.path(), 1, Arc::new(Resources::default()));
    let mut local = attach(&mut host, 2);
    let block = vec![5; IMAGE_BYTES as usize];
    for id in 0..7 {
        write(&mut local, id, 0, &block);
    }
    drained(&mut local, 7);
    let unused = local.prepare(Kind::Write(BLOCK_SIZE)).unwrap().unwrap();
    let (entered, observed) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    host.shared.control.lock().unwrap().rotation = Some(Pause { entered, resume });
    assert!(local.prepare(Kind::Write(block.len())).unwrap().is_none());
    assert!(matches!(
        observed.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    assert_eq!(local.report()["wal_window"]["unsubmitted"], 1);
    assert_eq!(local.admitted, 7);
    drop(unused);
    observed.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(local.report()["wal_window"]["unsubmitted"], 0);
    release.send(()).unwrap();
    write(&mut local, 7, 0, &block);
    assert_eq!(local.admitted, 8);
    drop(local);
    shutdown(host);
}

#[test]
fn paused_shared_attachment_gets_a_synced_fresh_epoch_from_the_worker() {
    let root = tempfile::tempdir().unwrap();
    let mut host = create(root.path(), 1, Arc::new(Resources::default()));
    let mut local = attach(&mut host, 2);
    let old = vec![5; IMAGE_BYTES as usize];
    write(&mut local, 0, 0, &old);
    drained(&mut local, 1);
    let epoch = local.status.epoch;
    let deadline = Instant::now() + Duration::from_secs(5);
    local.pause(deadline).unwrap();
    let fresh = local.new_attachment(deadline).unwrap();
    assert!(fresh.epoch > epoch);
    assert_eq!((fresh.published, fresh.durable), (1, 1));
    assert!(!fresh.rotating);
    local.resume().unwrap();
    read(&mut local, 1, &old);
    write(&mut local, 2, 0, &[7; BLOCK_SIZE]);
    drained(&mut local, 2);
    drop(local);
    shutdown(host);
}
