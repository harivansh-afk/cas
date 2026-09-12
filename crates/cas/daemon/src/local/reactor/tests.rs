use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::*;

#[derive(Clone, Copy)]
enum Case {
    Reorder { short: bool, pause: bool },
    Cohort { fail_sync: bool },
}

pub(super) struct Control {
    case: Case,
    release: AtomicBool,
    maximum: AtomicU64,
    first_seen: AtomicBool,
    sync_seen: AtomicBool,
}

impl Control {
    pub fn released(&self) -> bool {
        self.release.load(Ordering::Acquire)
    }

    pub fn entry(&self, work: &Work, entry: squeue::Entry) -> squeue::Entry {
        match (self.case, work) {
            (Case::Reorder { short: true, .. }, Work::Append(append))
                if append.submission.batch().envelope().first == 1 =>
            {
                // Persist only A's header; the CQE still requires its full length.
                opcode::Write::new(
                    types::Fd(append.submission.file().as_raw_fd()),
                    append.submission.batch().bytes().as_ptr(),
                    BLOCK_SIZE as u32,
                )
                .offset(append.submission.offset())
                .build()
            }
            (Case::Cohort { fail_sync: true }, Work::Fence(fence)) if fence.syncing => {
                opcode::Fsync::new(types::Fd(-1))
                    .flags(types::FsyncFlags::DATASYNC)
                    .build()
            }
            _ => entry,
        }
    }

    pub fn hold(&self, work: &Work) -> bool {
        match work {
            Work::Append(append) => {
                let envelope = append.submission.batch().envelope();
                self.maximum.fetch_max(envelope.last, Ordering::Release);
                if envelope.first == 1 {
                    self.first_seen.store(true, Ordering::Release);
                    matches!(self.case, Case::Reorder { .. }) && !self.released()
                } else {
                    false
                }
            }
            Work::Fence(fence) if fence.syncing => {
                self.sync_seen.store(true, Ordering::Release);
                matches!(self.case, Case::Cohort { .. }) && !self.released()
            }
            _ => false,
        }
    }
}

fn append(shared: &Shared, id: u64, queue: u16, value: u8) -> Command {
    let credits = shared
        .pools
        .append
        .reserve(Amount {
            bytes: MAX_BATCH_BYTES,
            requests: 0,
        })
        .unwrap();
    let permit = shared.reserve(Kind::Write(BLOCK_SIZE)).unwrap();
    let mut builder = Builder::new(MAX_REQUEST_BYTES as u64, MAX_REQUEST_BYTES).unwrap();
    builder
        .write(
            RequestId {
                serial: id,
                attachment: 1,
                queue,
                head: id as u16,
            },
            0,
            BLOCK_SIZE,
            |bytes| {
                bytes.fill(value);
                Ok(())
            },
        )
        .unwrap();
    let mut writes = reserved_vec(MAX_DESCRIPTORS, &shared.metadata).unwrap();
    writes.push(Write {
        id,
        data: Written::Data(BLOCK_SIZE),
        permit,
    });
    Command::Append(Packing {
        builder,
        credits,
        writes,
    })
}

fn operation(shared: &Shared, id: u64, boundary: u64, operation: Operation) -> Command {
    let kind = match &operation {
        Operation::Read { buffer, .. } => Kind::Read(buffer.as_slice().len()),
        Operation::Flush => Kind::Control,
        Operation::Write { .. } => unreachable!(),
    };
    Command::Io(Io {
        id,
        boundary,
        operation,
        permit: shared.reserve(kind).unwrap(),
    })
}

struct Run {
    directory: tempfile::TempDir,
    control: Arc<Control>,
    output: mailbox::Receiver<Response>,
    thread: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
    paused: Option<mailbox::Receiver<io::Result<append::Status>>>,
}

impl Run {
    fn start(short: bool) -> Self {
        Self::start_with_pause(short, false)
    }

    fn start_with_pause(short: bool, pause: bool) -> Self {
        Self::spawn(Case::Reorder { short, pause })
    }

    fn spawn(case: Case) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let config = append::Config {
            store: [1; 16],
            image: [2; 16],
            image_bytes: MAX_REQUEST_BYTES as u64,
            segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
        };
        let log = Log::create(
            directory.path().join("log"),
            config,
            append::Limits::default(),
        )
        .unwrap();
        let shared = Shared::new(log.status());
        let (output, receiver) = mailbox::bounded(136, &shared.metadata).unwrap();
        let worker = Worker {
            log,
            port: None,
            output,
            wake: Wake(EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap()),
            shared: Arc::clone(&shared),
        };
        let (sender, input) = mailbox::bounded(8, &shared.metadata).unwrap();
        for id in 1..=2 {
            sender
                .try_send(append(&shared, id, (id - 1) as u16, id as u8))
                .unwrap();
        }
        if matches!(case, Case::Cohort { .. }) {
            sender
                .try_send(operation(&shared, 3, 2, Operation::Flush))
                .unwrap();
            sender
                .try_send(operation(
                    &shared,
                    4,
                    2,
                    Operation::Read {
                        offset: 0,
                        buffer: AlignedBuffer::new(BLOCK_SIZE),
                    },
                ))
                .unwrap();
            sender.try_send(append(&shared, 5, 0, 3)).unwrap();
            sender
                .try_send(operation(&shared, 6, 3, Operation::Flush))
                .unwrap();
        }
        let paused = matches!(case, Case::Reorder { pause: true, .. }).then(|| {
            let (done, result) = mailbox::bounded(1, &shared.metadata).unwrap();
            sender
                .try_send(Command::Pause {
                    done,
                    _permit: shared.reserve(Kind::Control).unwrap(),
                })
                .unwrap();
            result
        });
        // Commands precede startup. Closing input disables idle sync; only
        // explicit FLUSHes can advance E in these controlled runs.
        drop(sender);
        let control = Arc::new(Control {
            case,
            release: AtomicBool::new(false),
            maximum: AtomicU64::new(0),
            first_seen: AtomicBool::new(false),
            sync_seen: AtomicBool::new(false),
        });
        let mut reactor = Reactor::new(
            worker,
            input,
            EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap(),
        )
        .unwrap();
        reactor.control = Some(Arc::clone(&control));
        let thread = thread::spawn(move || reactor.run());
        Self {
            directory,
            control,
            output: receiver,
            thread: Some(thread),
            shared,
            paused,
        }
    }

    fn wait_for_b(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.control.maximum.load(Ordering::Acquire) < 2
            || !self.control.first_seen.load(Ordering::Acquire)
        {
            assert!(Instant::now() < deadline, "both CQEs were not observed");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn release(&mut self) {
        self.control.release.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.control.release.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
fn held_sync_keeps_a_finite_prefix_while_reads_progress_and_later_writes_wait() {
    for fail_sync in [false, true] {
        let mut run = Run::spawn(Case::Cohort { fail_sync });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !run.control.sync_seen.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "sync CQE was not observed");
            thread::sleep(Duration::from_millis(1));
        }
        // Queue 0 and 1's overlapping writes publish before the held sync.
        // The boundary-2 read can complete while that cohort is still pending.
        for id in [1, 2, 4] {
            let (completion, _) = run.output.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_eq!(completion.id, id);
            assert!(completion.result.is_ok());
            if id == 4 {
                let CompletionData::Read(bytes) = &completion.data else {
                    panic!("expected read completion")
                };
                assert_eq!(bytes.as_slice(), &[2; BLOCK_SIZE]);
            }
        }
        assert!(matches!(
            run.output.try_recv(),
            Err(mailbox::TryRecvError::Empty)
        ));
        assert_eq!(run.shared.final_status.lock().unwrap().published, 2);
        assert_eq!(run.shared.health.lock().unwrap().durable, 0);
        assert_eq!(run.shared.final_status.lock().unwrap().issued, 2);
        assert_eq!(run.shared.metrics.lock().unwrap().batches_submitted, 2);
        // The queued later batch retains its allocation without submitting it.
        assert_eq!(
            run.shared.pools.append.usage().current.bytes,
            MAX_BATCH_BYTES
        );
        assert!(run.shared.pools.control.usage().current.bytes >= BLOCK_SIZE);

        run.release();
        for id in [3, 5, 6] {
            let (completion, status) = run.output.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_eq!(completion.id, id);
            assert_eq!(completion.result.is_err(), fail_sync);
            if fail_sync {
                assert_eq!(status.published, 2);
                assert_eq!(status.durable, 0);
                assert!(run.shared.health.lock().unwrap().failure.is_some());
            } else {
                assert_eq!(status.published, if id == 3 { 2 } else { 3 });
                assert_eq!(status.durable, if id == 6 { 3 } else { 2 });
            }
        }
        let metrics = run.shared.metrics.lock().unwrap();
        assert_eq!(metrics.batches_submitted, if fail_sync { 2 } else { 3 });
        assert_eq!(metrics.io_queued, metrics.io_completed);
        assert_eq!(metrics.io_queued, if fail_sync { 5 } else { 8 });
        assert_eq!(
            run.control.maximum.load(Ordering::Acquire),
            if fail_sync { 2 } else { 3 }
        );
        assert_eq!(run.shared.pools.append.usage().current.bytes, 0);
        assert_eq!(run.shared.pools.control.usage().current, Amount::default());
        assert_eq!(run.shared.pools.read.usage().current, Amount::default());
        assert_eq!(run.shared.pools.requests.usage().current, Amount::default());
    }
}

#[test]
fn observed_b_completion_cannot_acknowledge_over_withheld_a() {
    let mut run = Run::start(false);
    run.wait_for_b();
    assert!(matches!(
        run.output.try_recv(),
        Err(mailbox::TryRecvError::Empty)
    ));
    assert_eq!(run.shared.final_status.lock().unwrap().published, 0);
    // The intentionally wrong maximum-completed rule claims 2 while the
    // independent completion oracle requires 0 until A is delivered.
    assert_eq!(run.control.maximum.load(Ordering::Acquire), 2);
    assert_ne!(
        run.control.maximum.load(Ordering::Acquire),
        run.shared.final_status.lock().unwrap().published
    );
    run.release();
    for serial in 1..=2 {
        let (completion, _) = run.output.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(completion.id, serial);
        assert!(completion.result.is_ok());
    }
    assert_eq!(run.shared.final_status.lock().unwrap().durable, 0);
    let mut recovered = Log::open_with_expected_prefix(
        run.directory.path().join("log"),
        append::Limits::default(),
        2,
    )
    .unwrap();
    let mut bytes = AlignedBuffer::new(BLOCK_SIZE);
    recovered.read_into(0, &mut bytes).unwrap();
    assert_eq!(bytes.as_slice(), &[2; BLOCK_SIZE]);
}

#[test]
fn short_a_then_complete_b_fails_without_publication_and_rejects_the_suffix() {
    let mut run = Run::start(true);
    run.wait_for_b();
    assert!(matches!(
        run.output.try_recv(),
        Err(mailbox::TryRecvError::Empty)
    ));
    run.release();
    for _ in 0..2 {
        let (completion, _) = run.output.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(completion.result.is_err());
    }
    assert_eq!(run.shared.final_status.lock().unwrap().published, 0);
    assert!(run.shared.final_status.lock().unwrap().failed);
    let path = run.directory.path().join("log");
    let segment = path.join("segment-00000000000000000001.v2");
    let before = std::fs::read(&segment).unwrap();
    assert!(matches!(
        Log::open_with_expected_prefix(&path, append::Limits::default(), 1),
        Err(append::Error::Prefix {
            recovered: 0,
            required: 1
        })
    ));
    assert_eq!(std::fs::read(&segment).unwrap(), before);
    let recovered = Log::open(&path, append::Limits::default()).unwrap();
    assert_eq!(recovered.status().published, 0);
    assert_eq!(recovered.status().rejected_bytes, 4 * BLOCK_SIZE as u64);
}

#[test]
fn pause_waits_for_the_oldest_owned_append_even_after_a_later_cqe() {
    let mut run = Run::start_with_pause(false, true);
    run.wait_for_b();
    assert!(matches!(
        run.paused.as_ref().unwrap().try_recv(),
        Err(mailbox::TryRecvError::Empty)
    ));
    assert_eq!(run.shared.pools.control.usage().current.requests, 1);
    run.release();
    let status = run.paused.take().unwrap().recv().unwrap().unwrap();
    assert_eq!(status.published, 2);
    assert_eq!(status.durable, 0);
    assert_eq!(run.shared.pools.control.usage().current.requests, 0);
    let metrics = run.shared.metrics.lock().unwrap();
    assert_eq!(metrics.io_queued, metrics.io_completed);
}

#[test]
fn timed_out_pause_preserves_owners_and_no_late_write_can_succeed() {
    let mut run = Run::start_with_pause(false, true);
    run.wait_for_b();
    assert!(matches!(
        run.paused
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_millis(20)),
        Err(mailbox::RecvTimeoutError::Timeout)
    ));
    // The frontend's deadline expires while A remains owned by storage.
    drop(run.paused.take());
    run.shared
        .health
        .lock()
        .unwrap()
        .fail("storage pause timed out".into());
    let contender = std::fs::File::open(
        run.directory
            .path()
            .join("log/segment-00000000000000000001.v2"),
    )
    .unwrap();
    assert!(matches!(
        contender.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    assert_ne!(run.shared.pools.append.usage().current.bytes, 0);
    assert_eq!(run.shared.final_status.lock().unwrap().published, 0);
    run.release();
    for _ in 0..2 {
        let (completed, _) = run.output.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(completed.result.is_err());
    }
    assert_eq!(run.shared.final_status.lock().unwrap().published, 0);
    assert_eq!(run.shared.pools.append.usage().current.bytes, 0);
    assert_eq!(run.shared.pools.requests.usage().current.requests, 0);
    contender.try_lock().unwrap();
}

#[test]
fn thirty_second_io_deadline_fails_before_releasing_the_withheld_owner() {
    let started = Instant::now();
    let mut run = Run::start(false);
    run.wait_for_b();
    let deadline = started + IO_DEADLINE + Duration::from_secs(5);
    while run.shared.health.lock().unwrap().failure.is_none() {
        assert!(
            Instant::now() < deadline,
            "IO timeout did not fail the image"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(started.elapsed() >= IO_DEADLINE);
    let (later, status) = run.output.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(later.id, 2);
    assert!(later.result.is_err());
    assert_eq!((status.published, status.durable), (0, 0));
    drop(later);
    assert_eq!(
        run.shared.pools.append.usage().current.bytes,
        MAX_BATCH_BYTES
    );
    let contender = std::fs::File::open(
        run.directory
            .path()
            .join("log/segment-00000000000000000001.v2"),
    )
    .unwrap();
    assert!(matches!(
        contender.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    run.release();
    let (older, status) = run.output.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(older.id, 1);
    assert!(older.result.is_err());
    assert_eq!((status.published, status.durable), (0, 0));
    drop(older);
    assert_eq!(run.shared.pools.append.usage().current.bytes, 0);
    assert_eq!(run.shared.pools.requests.usage().current.requests, 0);
    contender.try_lock().unwrap();
}
