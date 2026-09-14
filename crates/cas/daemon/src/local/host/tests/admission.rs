use super::*;

#[test]
fn staging_reopen_wakes_a_blocked_frontend_without_an_io_credit_release() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 1, Arc::clone(&resources));
    let mut attachment = host.images[0].take().unwrap();
    let frontend = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap();
    host.shared
        .admission
        .bind(
            0,
            frontend.try_clone().unwrap(),
            EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap(),
        )
        .unwrap();
    // A prospective reservation closes the gate without allocating any disk.
    let capacity = host.shared.staging.status(0).unwrap().1.capacity;
    assert!(cas_core::space::Staging::reserve(&host.shared.staging, 0, capacity).is_err());
    assert!(host.shared.staging.status(0).unwrap().1.stopped);
    let ticket = attachment
        .port
        .fair()
        .ticket(Kind::Write(BLOCK_SIZE))
        .unwrap()
        .unwrap();
    drop(ticket.turn().unwrap().unwrap());
    assert_eq!(
        frontend.read().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    // Deliver the actual reclamation receipt through the reactor's production
    // handler, with no other worker, completion, queue kick or admission timer.
    let receipt = attachment
        .log
        .select_reclamation(None, Arc::clone(&resources.compaction))
        .unwrap()
        .run()
        .unwrap();
    let (events, receiver) = mailbox::bounded(1, &resources.metadata).unwrap();
    attachment.port.events = receiver;
    let (reply, replies) = mailbox::bounded(1, &resources.metadata).unwrap();
    attachment.port.reply = reply;
    events.try_send(Event::Reclaimed(receipt)).unwrap();
    attachment
        .port
        .poll(&mut attachment.log, Instant::now(), true)
        .unwrap();
    assert!(matches!(replies.try_recv().unwrap(), Reply::Applied));
    assert!(!host.shared.staging.status(0).unwrap().1.stopped);
    assert_eq!(frontend.read().unwrap(), 1);
    ticket.turn().unwrap().unwrap().commit();
    drop((ticket, attachment, events, replies));
    shutdown(host);
    assert_eq!(resources.metadata.usage().current.bytes, 0);
}

#[test]
fn full_index_waits_for_compaction_without_rotating_or_assigning_mutations() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create_with_limits(
        root.path(),
        2,
        Arc::clone(&resources),
        MAX_REQUEST_BYTES as u64,
        append::Limits {
            intervals: 126,
            ..append::Limits::default()
        },
    );
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    let (entered, observed) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    host.shared.control.lock().unwrap().compaction = Some(Pause { entered, resume });
    let mut expected = vec![0; MAX_REQUEST_BYTES];
    write(&mut first, 0, 0, &[1; BLOCK_SIZE]);
    expected[..BLOCK_SIZE].fill(1);
    observed.recv_timeout(Duration::from_secs(3)).unwrap();
    for id in 1..125 {
        let bytes = [id as u8 + 1; BLOCK_SIZE];
        let start = id * BLOCK_SIZE;
        write(&mut first, id as u64, start, &bytes);
        expected[start..start + BLOCK_SIZE].copy_from_slice(&bytes);
    }
    assert!(first.prepare(Kind::Write(BLOCK_SIZE)).unwrap().is_none());
    assert_eq!(first.admitted, 125);
    assert_eq!(first.report()["wal_window"]["index_pressure"], true);
    assert_eq!(first.report()["wal_window"]["rotation_wanted"], false);
    assert_eq!(first.report()["status"]["segments"], 1);
    read(&mut first, 125, &expected);
    let permit = first.prepare(Kind::Control).unwrap().unwrap();
    first.enqueue(126, Operation::Flush, permit).unwrap();
    completed(&mut first);
    assert_eq!(first.status.durable, 125);
    write(&mut second, 0, 0, &[7; BLOCK_SIZE]);
    read_at(&mut second, 1, 0, &[7; BLOCK_SIZE]);
    assert!(first.prepare(Kind::Write(BLOCK_SIZE)).unwrap().is_none());
    assert_eq!(first.admitted, 125);
    release.send(()).unwrap();
    write(&mut first, 127, 0, &[9; BLOCK_SIZE]);
    expected[..BLOCK_SIZE].fill(9);
    assert_eq!(first.admitted, 126);
    drained(&mut first, 126);
    assert_eq!(first.report()["status"]["segments"], 1);
    read(&mut first, 128, &expected);
    drop((first, second));
    shutdown(host);
    assert_eq!(resources.metadata.usage().current.bytes, 0);
}

#[test]
fn background_metrics_observe_real_compaction_io_and_manifest_work() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 1, Arc::clone(&resources));
    let totals = host.shared.compaction[0].clone();
    let mut local = attach(&mut host, 2);
    write(&mut local, 0, 0, &[7; BLOCK_SIZE]);
    drained(&mut local, 1);
    drop(local);
    shutdown(host); // The final worker turn has published its operation counters.
    let measured = *totals.lock().unwrap();
    assert_eq!(measured.manifest_changes, 1);
    assert!(measured.manifest_written_pages > 0);
    assert!(measured.manifest_allocated_bytes > 0);
    assert!(measured.exchange_calls > 0);
    assert!(measured.operations.read.calls > 0);
    assert!(measured.operations.write.calls > 0);
    assert!(measured.operations.sync.calls > 0);
    assert!(measured.operations.punch.calls > 0);
    assert_eq!(measured.operations.hash.requested_bytes, BLOCK_SIZE as u64);
    assert!(measured.operations.buffer_zero.requested_bytes >= measured.manifest_allocated_bytes);
    assert!(measured.operations.scheduler_wait.calls > 0);
    drop(totals);
    assert_eq!(resources.metadata.usage().current.bytes, 0);
}
