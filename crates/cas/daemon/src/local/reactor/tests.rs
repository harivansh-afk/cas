use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::*;

pub(super) struct Control {
    pub short: bool,
    release: AtomicBool,
    maximum: AtomicU64,
    first_seen: AtomicBool,
}

impl Control {
    pub fn released(&self) -> bool {
        self.release.load(Ordering::Acquire)
    }
    pub fn observe(&self, last: u64) {
        self.maximum.fetch_max(last, Ordering::Release);
        if last == 1 {
            self.first_seen.store(true, Ordering::Release);
        }
    }
}

struct Run {
    directory: tempfile::TempDir,
    control: Arc<Control>,
    output: mpsc::Receiver<Response>,
    thread: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
    paused: Option<mpsc::Receiver<io::Result<append::Status>>>,
}

impl Run {
    fn start(short: bool) -> Self {
        Self::start_with_pause(short, false)
    }

    fn start_with_pause(short: bool, pause: bool) -> Self {
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
        let pools = &shared.pools;
        let (output, receiver) = mpsc::channel();
        let worker = Worker {
            log,
            output,
            wake: Wake(EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap()),
            shared: Arc::clone(&shared),
        };
        let (sender, input) = mpsc::sync_channel(4);
        for serial in 1..=2 {
            let credits = pools
                .append
                .reserve(Amount {
                    bytes: MAX_BATCH_BYTES,
                    requests: 0,
                })
                .unwrap();
            let permit = Permit {
                _request: pools
                    .requests
                    .reserve(Amount {
                        bytes: 0,
                        requests: 1,
                    })
                    .unwrap(),
                _read: None,
            };
            let mut builder = Builder::new(config.image_bytes, MAX_REQUEST_BYTES).unwrap();
            builder
                .write(
                    RequestId {
                        serial,
                        attachment: 1,
                        queue: 0,
                        head: serial as u16,
                    },
                    0,
                    BLOCK_SIZE,
                    |bytes| {
                        bytes.fill(serial as u8);
                        Ok(())
                    },
                )
                .unwrap();
            sender
                .send(Command::Append(Packing {
                    builder,
                    credits,
                    writes: vec![Write {
                        id: serial,
                        bytes: BLOCK_SIZE,
                        permit,
                    }],
                }))
                .unwrap();
        }
        let paused = pause.then(|| {
            let (done, result) = mpsc::sync_channel(1);
            sender
                .send(Command::Pause {
                    done,
                    _permit: shared.reserve(Kind::Control).unwrap(),
                })
                .unwrap();
            result
        });
        // Both appends precede startup. Closing input disables idle sync, so the
        // test also checks recovery of completed, unsynchronized host writes.
        drop(sender);
        let control = Arc::new(Control {
            short,
            release: AtomicBool::new(false),
            maximum: AtomicU64::new(0),
            first_seen: AtomicBool::new(false),
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
fn observed_b_completion_cannot_acknowledge_over_withheld_a() {
    let mut run = Run::start(false);
    run.wait_for_b();
    assert!(matches!(
        run.output.try_recv(),
        Err(mpsc::TryRecvError::Empty)
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
        Err(mpsc::TryRecvError::Empty)
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
        Err(mpsc::TryRecvError::Empty)
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
