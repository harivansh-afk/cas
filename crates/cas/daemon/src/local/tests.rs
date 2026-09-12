use super::*;
use cas_core::aligned::AlignedBuffer;
use vmm_sys_util::eventfd::{EFD_CLOEXEC, EFD_NONBLOCK};

fn open(path: &Path) -> Local {
    let event = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap();
    Local::open(path, Some(8 * MAX_REQUEST_BYTES as u64), &event).unwrap()
}

fn concurrent(path: &Path) -> Local {
    let event = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap();
    Local::open_with_execution(
        path,
        Some(8 * MAX_REQUEST_BYTES as u64),
        &event,
        Execution::Concurrent,
    )
    .unwrap()
}

fn write(local: &mut Local, id: u64, offset: u64, length: usize, value: u8) {
    let permit = local.prepare(Kind::Write(length)).unwrap().unwrap();
    local
        .gather(
            id,
            QueueHead {
                queue: 0,
                head: id as u16,
            },
            offset,
            length,
            permit,
            |bytes| {
                bytes.fill(value);
                Ok(())
            },
        )
        .unwrap();
    local.seal().unwrap();
}

fn read(local: &mut Local, id: u64, offset: u64, length: usize) {
    let permit = local.prepare(Kind::Read(length)).unwrap().unwrap();
    local
        .enqueue(
            id,
            Operation::Read {
                offset,
                buffer: AlignedBuffer::new(length),
            },
            permit,
        )
        .unwrap();
}

#[test]
fn concurrent_appends_flush_and_read_use_owned_kernel_io() {
    let directory = tempfile::tempdir().unwrap();
    let mut local = concurrent(&directory.path().join("store"));
    let gate = Arc::clone(&local.shared.health);
    let stalled = gate.lock().unwrap();
    for id in 0..3 {
        write(&mut local, id, 0, BLOCK_SIZE, id as u8 + 1);
    }
    let permit = local.prepare(Kind::Control).unwrap().unwrap();
    local.enqueue(3, Operation::Flush, permit).unwrap();
    read(&mut local, 4, 0, BLOCK_SIZE);
    drop(stalled);
    let mut writes = Vec::new();
    for _ in 0..5 {
        let completed = local.receive(true).unwrap().unwrap();
        assert!(completed.result.is_ok(), "{:?}", completed.result);
        match completed.data {
            CompletionData::Write { .. } => writes.push(completed.id),
            CompletionData::Read(buffer) => assert_eq!(buffer.as_slice(), &[3; BLOCK_SIZE]),
            CompletionData::Flush => assert_eq!(local.status.durable, 3),
        }
    }
    assert_eq!(writes, [0, 1, 2]);
    let report = local.report();
    assert!(report["metrics"]["peak_awaiting_cqe"].as_u64().unwrap() >= 2);
    assert_eq!(
        report["metrics"]["io_queued"],
        report["metrics"]["io_completed"]
    );
    assert_eq!(report["append"]["current"]["bytes"], 0);
    assert_eq!(report["read"]["current"]["bytes"], 0);
    assert_eq!(report["control"]["current"]["bytes"], 0);
    assert_eq!(report["requests"]["current"]["requests"], 0);
}

#[test]
fn concurrent_flush_keeps_its_boundary_while_later_writes_wait() {
    let directory = tempfile::tempdir().unwrap();
    let mut local = concurrent(&directory.path().join("store"));
    let gate = Arc::clone(&local.shared.health);
    let stalled = gate.lock().unwrap();
    write(&mut local, 0, 0, BLOCK_SIZE, 1);
    let permit = local.prepare(Kind::Control).unwrap().unwrap();
    local.enqueue(1, Operation::Flush, permit).unwrap();
    write(&mut local, 2, 0, BLOCK_SIZE, 2);
    read(&mut local, 3, 0, BLOCK_SIZE);
    drop(stalled);
    let mut writes = Vec::new();
    for _ in 0..4 {
        let completed = local.receive(true).unwrap().unwrap();
        assert!(completed.result.is_ok(), "{:?}", completed.result);
        if completed.id == 1 {
            assert_eq!(
                (
                    local.status.published,
                    local.status.durable,
                    local.status.issued
                ),
                (1, 1, 1)
            );
        }
        match completed.data {
            CompletionData::Write { .. } => writes.push(completed.id),
            CompletionData::Read(buffer) => assert_eq!(buffer.as_slice(), &[2; BLOCK_SIZE]),
            CompletionData::Flush => (),
        }
    }
    assert_eq!(writes, [0, 2]);
    assert!(
        local.report()["metrics"]["cohort_pause_ns"]
            .as_u64()
            .unwrap()
            > 0
    );
}

#[test]
fn concurrent_partial_reads_verify_original_payloads_and_preserve_overwrites() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store");
    let mut local = concurrent(&path);
    write(&mut local, 0, 0, 2 * BLOCK_SIZE, 1);
    write(&mut local, 1, 0, BLOCK_SIZE, 2);
    read(&mut local, 2, 0, 2 * BLOCK_SIZE);
    for _ in 0..3 {
        let completed = local.receive(true).unwrap().unwrap();
        assert!(completed.result.is_ok());
        if let CompletionData::Read(buffer) = completed.data {
            assert_eq!(&buffer.as_slice()[..BLOCK_SIZE], &[2; BLOCK_SIZE]);
            assert_eq!(&buffer.as_slice()[BLOCK_SIZE..], &[1; BLOCK_SIZE]);
        }
    }
    use std::os::unix::fs::FileExt;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path.join("segment-00000000000000000001.v2"))
        .unwrap();
    file.write_all_at(&[0xff], 2 * BLOCK_SIZE as u64).unwrap();
    read(&mut local, 3, BLOCK_SIZE as u64, BLOCK_SIZE);
    let completed = local.receive(true).unwrap().unwrap();
    assert!(completed.result.is_err());
    assert!(local.shared.health.lock().unwrap().failure.is_some());
}

#[test]
fn guest_gathers_share_the_final_batch_and_release_after_completion() {
    for writes in [1, 32] {
        let directory = tempfile::tempdir().unwrap();
        let mut local = open(&directory.path().join("store"));
        let mut allocation = 0;
        for id in 0..writes {
            let permit = local.prepare(Kind::Write(BLOCK_SIZE)).unwrap().unwrap();
            let current = local.packing.as_ref().unwrap().builder.allocation_address();
            if id == 0 {
                allocation = current;
            }
            assert_eq!(current, allocation);
            local
                .gather(
                    id,
                    QueueHead {
                        queue: 0,
                        head: id as u16,
                    },
                    id * BLOCK_SIZE as u64,
                    BLOCK_SIZE,
                    permit,
                    |destination| {
                        assert_eq!(
                            destination.as_ptr() as usize,
                            allocation + (id as usize + 1) * BLOCK_SIZE
                        );
                        // Two guest spans gather straight into adjacent final-buffer slices.
                        destination[..512].copy_from_slice(&[1; 512]);
                        destination[512..].fill(2);
                        Ok(())
                    },
                )
                .unwrap();
        }
        let flush = local.prepare(Kind::Control).unwrap().unwrap();
        local.enqueue(writes, Operation::Flush, flush).unwrap();
        for id in 0..writes {
            let completion = local.receive(true).unwrap().unwrap();
            assert_eq!(completion.id, id);
            assert!(completion.result.is_ok());
            drop(completion);
            assert_eq!(
                local.shared.pools.requests.usage().current.requests,
                (writes - id - 1) as usize
            );
        }
        assert!(local.receive(true).unwrap().unwrap().result.is_ok());
        let report = local.report();
        assert_eq!(
            report["metrics"]["gathered_bytes"],
            writes * BLOCK_SIZE as u64
        );
        assert_eq!(report["metrics"]["batches_submitted"], 1);
        assert_eq!(report["metrics"]["allocation_identity_checks"], 1);
        assert_eq!(report["metrics"]["allocations_released"], 1);
        assert_eq!(report["append"]["current"]["bytes"], 0);
        assert_eq!(report["requests"]["current"]["requests"], 0);
        assert_eq!(report["control"]["current"]["requests"], 0);
        assert_eq!(
            local.status.encoded_bytes - BLOCK_SIZE as u64,
            (writes + 2) * BLOCK_SIZE as u64
        );
    }
}

#[test]
fn allocation_cap_blocks_before_gather_but_reserved_flush_still_enters() {
    let directory = tempfile::tempdir().unwrap();
    let mut local = open(&directory.path().join("store"));
    let gate = Arc::clone(&local.shared.health);
    let stalled = gate.lock().unwrap();
    for id in 0..7 {
        let permit = local
            .prepare(Kind::Write(MAX_REQUEST_BYTES))
            .unwrap()
            .unwrap();
        local
            .gather(
                id,
                QueueHead {
                    queue: 0,
                    head: id as u16,
                },
                0,
                MAX_REQUEST_BYTES,
                permit,
                |bytes| {
                    bytes.fill(id as u8);
                    Ok(())
                },
            )
            .unwrap();
        local.seal().unwrap();
    }
    assert!(
        local
            .prepare(Kind::Write(MAX_REQUEST_BYTES))
            .unwrap()
            .is_none()
    );
    assert!(local.packing.is_none());
    assert_eq!(local.shared.pools.requests.usage().current.requests, 7);
    assert_eq!(
        local.shared.pools.append.usage().current.bytes,
        7 * MAX_BATCH_BYTES
    );
    assert!(local.shared.pools.append.usage().peak.bytes <= 8 * MAX_REQUEST_BYTES);
    let flush = local.prepare(Kind::Control).unwrap().unwrap();
    local.enqueue(7, Operation::Flush, flush).unwrap();
    drop(stalled);
    for id in 0..7 {
        let completed = local.receive(true).unwrap().unwrap();
        assert_eq!(completed.id, id as u64);
        assert!(completed.result.is_ok());
    }
    assert!(local.receive(true).unwrap().unwrap().result.is_ok());
    assert_eq!(local.shared.pools.append.usage().current.bytes, 0);
    assert_eq!(local.shared.pools.requests.usage().current.requests, 0);
    assert_eq!(local.status.durable, 7);
}

#[test]
fn ordered_write_flush_overwrite_and_read_preserve_barriers() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store");
    let mut local = open(&path);
    let first = local.prepare(Kind::Write(BLOCK_SIZE)).unwrap().unwrap();
    local
        .gather(
            0,
            QueueHead { queue: 0, head: 0 },
            0,
            BLOCK_SIZE,
            first,
            |bytes| {
                bytes.fill(1);
                Ok(())
            },
        )
        .unwrap();
    let flush = local.prepare(Kind::Control).unwrap().unwrap();
    local.enqueue(1, Operation::Flush, flush).unwrap();
    let second = local.prepare(Kind::Write(BLOCK_SIZE)).unwrap().unwrap();
    local
        .gather(
            2,
            QueueHead { queue: 0, head: 2 },
            0,
            BLOCK_SIZE,
            second,
            |bytes| {
                bytes.fill(2);
                Ok(())
            },
        )
        .unwrap();
    let read = local.prepare(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    local
        .enqueue(
            3,
            Operation::Read {
                offset: 0,
                buffer: AlignedBuffer::new(BLOCK_SIZE),
            },
            read,
        )
        .unwrap();
    for id in 0..4 {
        let completed = local.receive(true).unwrap().unwrap();
        assert_eq!(completed.id, id);
        assert!(completed.result.is_ok());
        if id == 1 {
            assert_eq!(local.status.durable, 1);
        }
        if let CompletionData::Read(buffer) = completed.data {
            assert_eq!(buffer.as_slice(), &[2; BLOCK_SIZE]);
        }
    }
    assert_eq!(local.shared.pools.read.usage().current.bytes, 0);
    drop(local);
    let recovered = Log::open_with_expected_prefix(&path, append::Limits::default(), 2).unwrap();
    assert_eq!(recovered.status().published, 2);
}

#[test]
fn failed_gather_and_disconnected_owner_release_without_an_extra_fence() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store");
    let mut local = open(&path);
    let permit = local.prepare(Kind::Write(BLOCK_SIZE)).unwrap().unwrap();
    assert!(
        local
            .gather(
                0,
                QueueHead { queue: 0, head: 0 },
                0,
                BLOCK_SIZE,
                permit,
                |bytes| {
                    bytes[..512].fill(1);
                    Err(io::Error::other("guest mapping vanished"))
                }
            )
            .is_err()
    );
    local.seal().unwrap();
    assert_eq!(local.shared.pools.append.usage().current.bytes, 0);
    let permit = local.prepare(Kind::Write(BLOCK_SIZE)).unwrap().unwrap();
    local
        .gather(
            0,
            QueueHead { queue: 0, head: 0 },
            0,
            BLOCK_SIZE,
            permit,
            |bytes| {
                bytes.fill(9);
                Ok(())
            },
        )
        .unwrap();
    let shared = Arc::clone(&local.shared);
    let pools = &shared.pools;
    // Drop drains the owned work even when no caller will consume completion.
    drop(local);
    assert_eq!(pools.append.usage().current.bytes, 0);
    assert_eq!(pools.requests.usage().current.requests, 0);
    assert_eq!(
        File::open(path.join("segment-00000000000000000001.v2"))
            .unwrap()
            .metadata()
            .unwrap()
            .len(),
        3 * BLOCK_SIZE as u64
    );
}

#[test]
fn returned_read_keeps_request_and_byte_credits_after_worker_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let mut local = open(&directory.path().join("store"));
    let shared = Arc::clone(&local.shared);
    let pools = &shared.pools;
    let permit = local.prepare(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    local
        .enqueue(
            0,
            Operation::Read {
                offset: 0,
                buffer: AlignedBuffer::new(BLOCK_SIZE),
            },
            permit,
        )
        .unwrap();
    let completion = local.receive(true).unwrap().unwrap();
    assert!(completion.result.is_ok());
    drop(local);
    assert_eq!(pools.requests.usage().current.requests, 1);
    assert_eq!(
        pools.read.usage().current.bytes,
        BLOCK_SIZE + MAX_REQUEST_BYTES
    );
    drop(completion);
    assert_eq!(pools.requests.usage().current.requests, 0);
    assert_eq!(pools.read.usage().current.bytes, 0);
}

#[test]
fn rejected_batch_and_following_read_return_each_owner_until_credits_are_released() {
    for disconnected in [false, true] {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut local = concurrent(&directory.path().join("log"));
        drop(local.sender.take());
        local.input_wake.as_ref().unwrap().write(1).unwrap();
        local.worker.take().unwrap().join().unwrap();
        let (sender, receiver) = mailbox::bounded(1, &local.shared.metadata).unwrap();
        sender.try_send(Command::Resume).unwrap();
        let receiver = (!disconnected).then_some(receiver);
        local.sender = Some(sender);
        let read = local.shared.reserve(Kind::Read(BLOCK_SIZE)).unwrap();
        for id in 0..2 {
            let permit = local.prepare(Kind::Write(BLOCK_SIZE)).unwrap().unwrap();
            local
                .gather(
                    id,
                    QueueHead {
                        queue: 0,
                        head: id as u16,
                    },
                    0,
                    BLOCK_SIZE,
                    permit,
                    |bytes| {
                        bytes.fill(id as u8);
                        Ok(())
                    },
                )
                .unwrap();
        }
        assert!(
            local
                .enqueue(
                    2,
                    Operation::Read {
                        offset: 0,
                        buffer: AlignedBuffer::new(BLOCK_SIZE)
                    },
                    read
                )
                .is_err()
        );
        let report = local.report();
        assert_eq!(report["append"]["current"]["bytes"], 0); // Rejected final batch is gone.
        assert_eq!(report["requests"]["current"]["requests"], 3);
        assert_eq!(
            report["read"]["current"]["bytes"],
            BLOCK_SIZE + MAX_REQUEST_BYTES
        );
        let mut completed = Vec::new();
        for id in 0..3 {
            let item = local.receive(true).unwrap().unwrap();
            assert_eq!(item.id, id);
            assert!(item.result.is_err());
            completed.push(item);
        }
        assert_eq!(local.report()["requests"]["current"]["requests"], 3);
        drop(completed);
        assert_eq!(local.report()["requests"]["current"]["requests"], 0);
        assert_eq!(local.report()["read"]["current"]["bytes"], 0);
        drop(receiver);
        local.stop().unwrap();
    }
}

#[test]
fn accepted_command_survives_an_already_pending_wake_notification() {
    let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let mut local = concurrent(&directory.path().join("log"));
    drop(local.sender.take());
    local.input_wake.as_ref().unwrap().write(1).unwrap();
    local.worker.take().unwrap().join().unwrap();
    let (sender, receiver) = mailbox::bounded(1, &local.shared.metadata).unwrap();
    local.sender = Some(sender);
    let wake = EventFd::new(EFD_NONBLOCK | EFD_CLOEXEC).unwrap();
    wake.write(u64::MAX - 1).unwrap();
    local.input_wake = Some(wake);
    let permit = local.prepare(Kind::Control).unwrap().unwrap();
    local.enqueue(0, Operation::Flush, permit).unwrap();
    assert_eq!(local.report()["control"]["current"]["requests"], 1);
    let command = receiver.try_recv().unwrap();
    assert_eq!(
        local.input_wake.as_ref().unwrap().read().unwrap(),
        u64::MAX - 1
    );
    command.reject("test worker retirement", &mut local.rejected);
    drop(local.receive(true).unwrap().unwrap());
    assert_eq!(local.report()["control"]["current"]["requests"], 0);
    local.stop().unwrap();
}

#[test]
fn pause_drains_prior_reads_and_writes_and_resume_restores_admission() {
    use std::time::{Duration, Instant};
    let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let mut local = concurrent(&directory.path().join("log"));
    write(&mut local, 0, 0, BLOCK_SIZE, 0x5a);
    let permit = local.prepare(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    local
        .enqueue(
            1,
            Operation::Read {
                offset: 0,
                buffer: AlignedBuffer::new(BLOCK_SIZE),
            },
            permit,
        )
        .unwrap();
    local
        .pause(Instant::now() + Duration::from_secs(5))
        .unwrap();
    assert!(local.prepare(Kind::Write(BLOCK_SIZE)).is_err());
    let before = local.report()["metrics"]["io_queued"].as_u64().unwrap();
    assert_eq!(local.report()["metrics"]["io_completed"], before);
    for _ in 0..2 {
        let completed = local.receive(true).unwrap().unwrap();
        completed.result.unwrap();
        if let CompletionData::Read(buffer) = completed.data {
            assert_eq!(buffer.as_slice(), &[0x5a; BLOCK_SIZE]);
        }
    }
    // Remain beyond the idle-sync interval without creating new kernel work.
    thread::sleep(Duration::from_millis(75));
    assert_eq!(local.report()["metrics"]["io_queued"], before);
    local.resume().unwrap();
    write(&mut local, 2, 0, BLOCK_SIZE, 0xa5);
    let permit = local.prepare(Kind::Control).unwrap().unwrap();
    local.enqueue(3, Operation::Flush, permit).unwrap();
    for _ in 0..2 {
        local.receive(true).unwrap().unwrap().result.unwrap();
    }
    local.stop().unwrap();
    assert_eq!(local.report()["status"]["durable"], 2);
    assert_eq!(local.report()["requests"]["current"]["requests"], 0);
}

#[test]
fn fresh_attachment_syncs_both_epochs_and_keeps_image_mutations_monotonic() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("log");
    let mut local = concurrent(&path);
    for generation in 1..=3 {
        assert_eq!(local.status.epoch, generation);
        write(
            &mut local,
            0,
            (generation - 1) * BLOCK_SIZE as u64,
            BLOCK_SIZE,
            generation as u8,
        );
        local.pause(Instant::now() + IO_DEADLINE).unwrap();
        local.receive(true).unwrap().unwrap().result.unwrap();
        if generation < 3 {
            let status = local.new_attachment(Instant::now() + IO_DEADLINE).unwrap();
            assert_eq!(
                (status.epoch, status.published, status.durable),
                (generation + 1, generation, generation)
            );
            assert_eq!(local.report()["control"]["current"]["requests"], 0);
            assert!(local.prepare(Kind::Write(BLOCK_SIZE)).is_err());
        }
        local.resume().unwrap();
    }
    let permit = local.prepare(Kind::Control).unwrap().unwrap();
    local.enqueue(1, Operation::Flush, permit).unwrap();
    local.receive(true).unwrap().unwrap().result.unwrap();
    local.stop().unwrap();
    drop(local);
    let mut log = Log::open(&path, append::Limits::default()).unwrap();
    let mut bytes = AlignedBuffer::new(3 * BLOCK_SIZE);
    log.read_into(0, &mut bytes).unwrap();
    for (index, chunk) in bytes
        .as_slice()
        .as_chunks::<BLOCK_SIZE>()
        .0
        .iter()
        .enumerate()
    {
        assert_eq!(chunk, &[index as u8 + 1; BLOCK_SIZE]);
    }
    assert_eq!((log.status().published, log.status().durable), (3, 3));
}

#[test]
fn channel_allocations_fail_before_worker_start_and_release_partial_startup() {
    let probe = Budget::new(Amount {
        bytes: MAX_REQUEST_BYTES,
        requests: 0,
    });
    let channel =
        mailbox::bounded::<Command>(pools::IMAGE_REQUESTS + pools::IMAGE_CONTROL + 1, &probe)
            .unwrap();
    let command_bytes = probe.usage().current.bytes;
    drop(channel);
    for bytes in [0, command_bytes] {
        let directory = tempfile::tempdir().unwrap();
        let log = create_log(&directory.path().join("log"), MAX_REQUEST_BYTES as u64).unwrap();
        let mut shared = Shared::new(log.status());
        let metadata = Budget::new(Amount { bytes, requests: 0 });
        Arc::get_mut(&mut shared).unwrap().metadata = Arc::clone(&metadata);
        let event = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap();
        let result = Local::from_log(log, &event, Execution::Concurrent, shared);
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::OutOfMemory));
        assert_eq!(metadata.usage().current, Amount::default());
        assert!(metadata.usage().rejected > 0);
    }
}

#[test]
fn all_request_completions_fit_without_frontend_consumption() {
    let directory = tempfile::tempdir().unwrap();
    let mut local = concurrent(&directory.path().join("log"));
    let metadata = Arc::clone(&local.shared.metadata);
    for id in 0..pools::IMAGE_REQUESTS as u64 {
        let permit = local.prepare(Kind::Write(BLOCK_SIZE)).unwrap().unwrap();
        local
            .gather(
                id,
                QueueHead {
                    queue: 0,
                    head: id as u16,
                },
                id * BLOCK_SIZE as u64,
                BLOCK_SIZE,
                permit,
                |bytes| {
                    bytes.fill(id as u8);
                    Ok(())
                },
            )
            .unwrap();
    }
    local.seal().unwrap();
    for id in pools::IMAGE_REQUESTS..pools::IMAGE_REQUESTS + pools::IMAGE_CONTROL {
        let permit = local.prepare(Kind::Control).unwrap().unwrap();
        local.enqueue(id as u64, Operation::Flush, permit).unwrap();
    }
    assert!(local.shared.reserve(Kind::Write(BLOCK_SIZE)).is_none());
    assert!(local.shared.reserve(Kind::Control).is_none());
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        drop(local.sender.take());
        local.input_wake.as_ref().unwrap().write(1).unwrap();
        local.worker.take().unwrap().join().unwrap();
        done.send(local).expect("test receiver retained");
    });
    let mut local = result.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(
        local.shared.pools.requests.usage().current.requests,
        pools::IMAGE_REQUESTS
    );
    assert_eq!(
        local.shared.pools.control.usage().current.requests,
        pools::IMAGE_CONTROL
    );
    for id in 0..pools::IMAGE_REQUESTS + pools::IMAGE_CONTROL {
        let completed = local.receive(false).unwrap().unwrap();
        assert_eq!(completed.id, id as u64);
        assert!(completed.result.is_ok());
        drop(completed);
    }
    assert_eq!(local.status.durable, pools::IMAGE_REQUESTS as u64);
    assert_eq!(
        local.shared.pools.requests.usage().current,
        Amount::default()
    );
    assert_eq!(
        local.shared.pools.control.usage().current,
        Amount::default()
    );
    drop(local);
    assert_eq!(metadata.usage().current, Amount::default());
}

#[test]
fn descriptor_metadata_denial_precedes_mutation_and_refunds_partial_admission() {
    let directory = tempfile::tempdir().unwrap();
    let log = create_log(&directory.path().join("log"), MAX_REQUEST_BYTES as u64).unwrap();
    let capacity = MAX_REQUEST_BYTES;
    let metadata = Budget::new(Amount {
        bytes: capacity,
        requests: 0,
    });
    let mut shared = Shared::new(log.status());
    Arc::get_mut(&mut shared).unwrap().metadata = Arc::clone(&metadata);
    let event = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap();
    let mut local = Local::from_log(log, &event, Execution::Concurrent, shared).unwrap();
    let baseline = metadata.usage().current;
    let held = metadata
        .reserve(Amount {
            bytes: capacity - baseline.bytes,
            requests: 0,
        })
        .unwrap();
    let denied = local.prepare(Kind::Write(BLOCK_SIZE));
    assert!(matches!(denied, Err(error) if error.kind() == io::ErrorKind::OutOfMemory));
    assert_eq!(local.admitted, 0);
    assert!(local.packing.is_none());
    assert_eq!(
        local.shared.pools.requests.usage().current,
        Amount::default()
    );
    assert_eq!(local.shared.pools.append.usage().current, Amount::default());
    drop(held);
    assert_eq!(metadata.usage().current, baseline);
    write(&mut local, 0, 0, BLOCK_SIZE, 0x57);
    let completion = local.receive(true).unwrap().unwrap();
    assert!(completion.result.is_ok());
    drop((completion, local));
    assert_eq!(metadata.usage().current, Amount::default());
}

#[test]
fn read_preparation_errors_return_every_owner_before_credit_release() {
    for exhaust_metadata in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut local = concurrent(&directory.path().join("log"));
        let shared = Arc::clone(&local.shared);
        let first = local.prepare(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
        let second = local.prepare(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
        let gate = shared.health.lock().unwrap();
        let held = exhaust_metadata.then(|| {
            shared
                .metadata
                .reserve(Amount {
                    bytes: 128 * MAX_REQUEST_BYTES - shared.metadata.usage().current.bytes,
                    requests: 0,
                })
                .unwrap()
        });
        let first_offset = if exhaust_metadata {
            0
        } else {
            local.status.image_bytes
        };
        for (id, offset, permit) in [(0, first_offset, first), (1, 0, second)] {
            local
                .enqueue(
                    id,
                    Operation::Read {
                        offset,
                        buffer: AlignedBuffer::new(BLOCK_SIZE),
                    },
                    permit,
                )
                .unwrap();
        }
        drop(gate);
        notify(local.input_wake.as_ref().unwrap()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut responses = Vec::new();
        while responses.len() < 2 {
            if let Some(response) = local.receive(false).unwrap() {
                assert_eq!(response.id, responses.len() as u64);
                assert!(response.result.is_err());
                assert!(shared.health.lock().unwrap().failure.is_some());
                responses.push(response);
            }
            assert!(
                Instant::now() < deadline,
                "read preparation lost a completion owner"
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(shared.pools.requests.usage().current.requests, 2);
        assert_eq!(
            shared.pools.read.usage().current.bytes,
            2 * (BLOCK_SIZE + MAX_REQUEST_BYTES)
        );
        assert_eq!(local.report()["metrics"]["io_queued"], 0);
        assert_eq!(local.status.published, 0);
        drop(responses);
        assert_eq!(shared.pools.requests.usage().current, Amount::default());
        assert_eq!(shared.pools.read.usage().current, Amount::default());
        drop((held, local));
        assert_eq!(shared.metadata.usage().current, Amount::default());
    }
}
