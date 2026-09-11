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
