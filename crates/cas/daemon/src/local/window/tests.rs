use super::*;

struct Fixture {
    _root: tempfile::TempDir,
    log: Log,
    shared: Arc<Shared>,
    window: BudgetArc<Window>,
    metadata: Arc<Budget>,
}

impl Fixture {
    fn new() -> Self {
        Self::with_intervals(append::Limits::default().intervals)
    }

    fn with_intervals(intervals: usize) -> Self {
        let root = tempfile::tempdir().unwrap();
        let log = Log::create(
            root.path().join("wal"),
            append::Config {
                store: [1; 16],
                image: [2; 16],
                image_bytes: MAX_REQUEST_BYTES as u64,
                segment_bytes: (MAX_REQUEST_BYTES + 4 * BLOCK_SIZE) as u64,
            },
            append::Limits {
                intervals,
                ..append::Limits::default()
            },
        )
        .unwrap();
        let metadata = Budget::new(Amount {
            bytes: 4096,
            requests: 0,
        });
        let window = Window::new(&log, None, &metadata).unwrap();
        let mut shared = Shared::new(log.status());
        Arc::get_mut(&mut shared).unwrap().window = Some(window.clone());
        Self {
            _root: root,
            log,
            shared,
            window,
            metadata,
        }
    }

    fn batch(&self, lengths: &[usize]) -> (Builder, Vec<Write>) {
        let mut builder = Builder::new(MAX_REQUEST_BYTES as u64, MAX_REQUEST_BYTES).unwrap();
        let mut writes = Vec::new();
        let mut offset = 0;
        for (index, &bytes) in lengths.iter().enumerate() {
            let permit = self.shared.reserve(Kind::Write(bytes)).unwrap();
            builder
                .write(
                    RequestId {
                        serial: index as u64 + 1,
                        attachment: self.log.status().epoch,
                        queue: 0,
                        head: index as u16,
                    },
                    offset,
                    bytes,
                    |output| {
                        output.fill(index as u8 + 1);
                        Ok(())
                    },
                )
                .unwrap();
            offset += bytes as u64;
            writes.push(Write {
                id: index as u64,
                bytes,
                permit,
            });
        }
        (builder, writes)
    }

    fn sync(&mut self) {
        let fence = self.window.fence(&mut self.log).unwrap();
        fence.write().unwrap();
        assert!(self.log.ready_to_sync(&fence).unwrap());
        fence.file().sync_data().unwrap();
        self.log.complete_sync(&fence).unwrap();
        assert!(!self.window.installed(&self.log).unwrap());
    }
}

#[test]
fn descriptor_tokens_cover_pending_publication_and_refund_without_mutation() {
    let mut f = Fixture::with_intervals(126);
    let (builder, mut writes) = f.batch(&[BLOCK_SIZE; 63]);
    assert_eq!(f.log.append_capacity(), 63);
    assert!(f.shared.reserve(Kind::Write(BLOCK_SIZE)).is_none());
    assert_eq!(f.log.status().issued, 0);
    assert!(f.window.status().index_pressure);
    assert!(!f.window.status().rotation_wanted);

    let submission = Window::append(&f.window, &mut f.log, builder, &mut writes).unwrap();
    assert_eq!(f.log.append_capacity(), 0);
    assert!(f.shared.reserve(Kind::Write(BLOCK_SIZE)).is_none());
    submission.write().unwrap();
    f.log.publish_append(&submission).unwrap();
    assert_eq!(f.log.status().intervals, 63);
    assert_eq!(f.log.append_capacity(), 31);
    // The stale sample stays conservative until the sequencer refreshes it.
    assert!(f.shared.reserve(Kind::Write(BLOCK_SIZE)).is_none());
    assert!(f.window.refresh(&f.log).unwrap());
    let mut tokens: Vec<_> = (0..31)
        .map(|_| f.shared.reserve(Kind::Write(BLOCK_SIZE)).unwrap())
        .collect();
    assert!(f.shared.reserve(Kind::Write(BLOCK_SIZE)).is_none());
    let point = f.log.position();
    drop(tokens.pop());
    let replacement = f.shared.reserve(Kind::Write(BLOCK_SIZE)).unwrap();
    assert_eq!(f.log.position(), point);
    assert_eq!(f.window.status().unsubmitted, 31);
    drop((tokens, replacement, writes, submission));
    assert_eq!(f.window.status().unsubmitted, 0);
    f.sync();
}

#[test]
fn empty_flush_before_maximum_admitted_write_preserves_both_fence_slots() {
    let mut f = Fixture::new();
    let (builder, mut writes) = f.batch(&[MAX_REQUEST_BYTES]);
    assert_eq!(
        f.window.status().used().unwrap(),
        f.log.config().segment_bytes
    );
    // This already queued empty FLUSH must not consume the write's own fence.
    f.sync();
    assert_eq!(f.log.status().issued, 0);
    assert_eq!(
        f.window.status().used().unwrap(),
        f.log.config().segment_bytes
    );
    let submission = Window::append(&f.window, &mut f.log, builder, &mut writes).unwrap();
    submission.write().unwrap();
    f.log.publish_append(&submission).unwrap();
    drop(submission);
    assert_eq!(f.window.status().unsubmitted, 0);
    assert!(f.shared.reserve(Kind::Write(BLOCK_SIZE)).is_none());
    f.sync();
    assert_eq!(f.log.position().end, f.log.config().segment_bytes);
    f.window.before_rotation(&f.log).unwrap();
    let rotation = f
        .log
        .prepare_rotation(append::RotationKind::Rollover)
        .unwrap();
    f.log.install_rotation(rotation.create().unwrap()).unwrap();
    assert!(f.window.installed(&f.log).unwrap());
    assert_eq!(f.log.status().published, 1);
    let permit = f.shared.reserve(Kind::Write(MAX_REQUEST_BYTES)).unwrap();
    drop(permit);
    let mut data = cas_core::aligned::AlignedBuffer::new(MAX_REQUEST_BYTES);
    f.log.read_into(0, &mut data).unwrap();
    assert!(data.as_slice().iter().all(|&byte| byte == 1));
}

#[test]
fn packed_headers_refund_capacity_and_unused_tokens_retain_one_charged_owner() {
    let mut f = Fixture::new();
    let charged = f.metadata.usage().current.bytes;
    let (builder, mut writes) = f.batch(&[BLOCK_SIZE, BLOCK_SIZE]);
    let reserved = f.window.status().used().unwrap();
    assert_eq!(f.metadata.usage().current.bytes, charged);
    let submission = Window::append(&f.window, &mut f.log, builder, &mut writes).unwrap();
    assert_eq!(
        f.window.status().used().unwrap(),
        reserved - BLOCK_SIZE as u64
    );
    assert!(writes.iter().all(|write| write.permit.window.is_none()));
    submission.write().unwrap();
    f.log.publish_append(&submission).unwrap();
    drop(submission);
    f.sync();
    let before = f.window.status().used();
    let unused = f.shared.reserve(Kind::Write(BLOCK_SIZE)).unwrap();
    assert_eq!(f.metadata.usage().current.bytes, charged);
    drop(unused);
    assert_eq!(f.window.status().used(), before);
    let unused = f.shared.reserve(Kind::Write(BLOCK_SIZE)).unwrap();
    let budget = Arc::clone(&f.metadata);
    drop(f);
    assert_eq!(budget.usage().current.bytes, charged);
    drop(unused);
    assert_eq!(budget.usage().current.bytes, 0);
}

#[test]
fn foreign_or_mismatched_tokens_and_unobserved_positions_fail_before_preparation() {
    for case in 0..3 {
        let mut f = Fixture::new();
        let other = Fixture::new();
        let (builder, mut writes) = if case == 0 {
            other.batch(&[BLOCK_SIZE])
        } else {
            f.batch(&[BLOCK_SIZE])
        };
        if case == 1 {
            writes[0].bytes *= 2;
        }
        if case == 2 {
            let fence = f.log.prepare_fence().unwrap(); // Bypass the host's sequencer deliberately.
            drop(fence);
        }
        let before = f.log.position();
        assert!(Window::append(&f.window, &mut f.log, builder, &mut writes).is_err());
        assert_eq!(f.log.position(), before);
        assert!(f.window.status().failed);
        drop(writes);
        assert_eq!(f.window.status().unsubmitted, 0);
    }
    let f = Fixture::new();
    let before = f.log.position();
    assert!(Window::new(&f.log, None, &Budget::new(Amount::default())).is_err());
    assert_eq!(f.log.position(), before);
}
