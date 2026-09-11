use super::*;
use cas_core::aligned::AlignedBuffer;
use vmm_sys_util::eventfd::{EFD_CLOEXEC, EFD_NONBLOCK};

fn open(path: &Path) -> Local {
    let event = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap();
    Local::open(path, Some(8 * MAX_REQUEST_BYTES as u64), &event).unwrap()
}

#[test]
fn guest_gathers_share_the_final_batch_and_release_after_completion() {
    for writes in [1, 32] {
        let directory = tempfile::tempdir().unwrap();
        let mut local = open(&directory.path().join("store"));
        let mut permits = Vec::new();
        let mut allocation = 0;
        for id in 0..writes {
            permits.push(local.prepare(Kind::Write(BLOCK_SIZE)).unwrap().unwrap());
            let current = local.packing.as_ref().unwrap().builder.allocation_address();
            if id == 0 {
                allocation = current;
            }
            assert_eq!(current, allocation);
            local
                .gather(
                    id,
                    id as u16,
                    id * BLOCK_SIZE as u64,
                    BLOCK_SIZE,
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
        local.enqueue(writes, Operation::Flush).unwrap();
        for (id, permit) in permits.into_iter().enumerate() {
            let completion = local.receive(true).unwrap().unwrap();
            assert_eq!(completion.id, id as u64);
            assert!(completion.result.is_ok());
            drop(permit);
        }
        assert!(local.receive(true).unwrap().unwrap().result.is_ok());
        drop(flush);
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
    let gate = Arc::clone(&local.health);
    let stalled = gate.lock().unwrap();
    let mut permits = Vec::new();
    for id in 0..7 {
        permits.push(
            local
                .prepare(Kind::Write(MAX_REQUEST_BYTES))
                .unwrap()
                .unwrap(),
        );
        local
            .gather(id, id as u16, 0, MAX_REQUEST_BYTES, |bytes| {
                bytes.fill(id as u8);
                Ok(())
            })
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
    assert_eq!(local.pools.requests.usage().current.requests, 7);
    assert_eq!(
        local.pools.append.usage().current.bytes,
        7 * MAX_BATCH_BYTES
    );
    assert!(local.pools.append.usage().peak.bytes <= 8 * MAX_REQUEST_BYTES);
    let flush = local.prepare(Kind::Control).unwrap().unwrap();
    local.enqueue(7, Operation::Flush).unwrap();
    drop(stalled);
    for (id, permit) in permits.into_iter().enumerate() {
        let completed = local.receive(true).unwrap().unwrap();
        assert_eq!(completed.id, id as u64);
        assert!(completed.result.is_ok());
        drop(permit);
    }
    assert!(local.receive(true).unwrap().unwrap().result.is_ok());
    drop(flush);
    assert_eq!(local.pools.append.usage().current.bytes, 0);
    assert_eq!(local.pools.requests.usage().current.requests, 0);
    assert_eq!(local.status.durable, 7);
}

#[test]
fn ordered_write_flush_overwrite_and_read_preserve_barriers() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store");
    let mut local = open(&path);
    let first = local.prepare(Kind::Write(BLOCK_SIZE)).unwrap().unwrap();
    local
        .gather(0, 0, 0, BLOCK_SIZE, |bytes| {
            bytes.fill(1);
            Ok(())
        })
        .unwrap();
    let flush = local.prepare(Kind::Control).unwrap().unwrap();
    local.enqueue(1, Operation::Flush).unwrap();
    let second = local.prepare(Kind::Write(BLOCK_SIZE)).unwrap().unwrap();
    local
        .gather(2, 2, 0, BLOCK_SIZE, |bytes| {
            bytes.fill(2);
            Ok(())
        })
        .unwrap();
    let read = local.prepare(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    local
        .enqueue(
            3,
            Operation::Read {
                offset: 0,
                buffer: AlignedBuffer::new(BLOCK_SIZE),
            },
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
    drop((first, flush, second, read));
    assert_eq!(local.pools.read.usage().current.bytes, 0);
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
            .gather(0, 0, 0, BLOCK_SIZE, |bytes| {
                bytes[..512].fill(1);
                Err(io::Error::other("guest mapping vanished"))
            })
            .is_err()
    );
    drop(permit);
    local.seal().unwrap();
    assert_eq!(local.pools.append.usage().current.bytes, 0);
    let permit = local.prepare(Kind::Write(BLOCK_SIZE)).unwrap().unwrap();
    local
        .gather(0, 0, 0, BLOCK_SIZE, |bytes| {
            bytes.fill(9);
            Ok(())
        })
        .unwrap();
    let pools = Arc::clone(&local.pools);
    // Drop drains the owned work even when no caller will consume completion.
    drop(local);
    drop(permit);
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
