use super::*;
use crate::deadline::Deadline;
use std::time::{Duration, Instant};
use vm_memory::GuestAddress;

#[test]
fn idle_frontend_deadline_fails_the_retained_attachment_without_guest_access() {
    let directory = tempfile::tempdir().unwrap();
    let mut backend = Backend::open_with_recovery(
        &directory.path().join("log"),
        BackendKind::LocalAsync,
        Some(BLOCK_SIZE as u64),
        true,
        Fault::default(),
    )
    .unwrap();
    backend
        .create_attachment(&VhostUserInflight {
            mmap_size: 0,
            mmap_offset: 0,
            num_queues: 1,
            queue_size: 128,
        })
        .unwrap();
    let deadline = Deadline::after(Duration::from_millis(20));
    backend.recovery_deadline = Some(deadline);
    backend.recovery_timer = Some(deadline.timer().unwrap());
    std::thread::sleep(Duration::from_millis(30));
    let (_, token) = backend.deadline_listener().unwrap();
    assert_eq!(
        backend
            .handle_event(token, vmm_sys_util::epoll::EventSet::IN, &[], 0)
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    let gate = backend.storage.completion_gate().unwrap();
    let state = gate.lock().unwrap();
    assert!(state.failure.is_some());
    let carrier = state.carrier.as_ref().unwrap();
    let (message, file) = carrier.export().unwrap();
    assert!(Carrier::attach(file, &message, carrier.identity(), BLOCK_SIZE as u64).is_err());
    assert!(backend.memory.is_none());
    assert_eq!(backend.pending_count(), 0);
}

#[test]
fn retained_read_uses_the_normal_owned_reactor_and_original_head() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("log");
    let mut log = local::create_log(&path, 0x10000).unwrap();
    let config = log.config();
    let mut builder =
        cas_core::append::format::Builder::new(config.image_bytes, BLOCK_SIZE).unwrap();
    builder
        .write(
            RequestId {
                serial: 1,
                attachment: 1,
                queue: 0,
                head: 3,
            },
            0,
            BLOCK_SIZE,
            |bytes| {
                bytes.fill(0x71);
                Ok(())
            },
        )
        .unwrap();
    log.append(builder).unwrap();
    log.flush().unwrap();
    drop(log);
    let mut carrier = Carrier::create(
        Geometry::new(1, 128).unwrap(),
        Identity {
            store: config.store,
            image: config.image,
            epoch: 1,
            attachment: 1,
        },
        config.image_bytes,
        0,
    )
    .unwrap();
    carrier.initialize_queue(0, 0, 0).unwrap();
    let request = cas_daemon::inflight::Request {
        kind: cas_daemon::inflight::Kind::Write,
        queue: 0,
        head: 3,
        available: 0,
        offset: 0,
        length: BLOCK_SIZE as u64,
    };
    let write = carrier.admit(request).unwrap();
    carrier.publish(1).unwrap();
    carrier.complete(write, 0, || Ok(())).unwrap();
    let read = carrier
        .admit(cas_daemon::inflight::Request {
            kind: cas_daemon::inflight::Kind::Read,
            head: 0,
            available: 1,
            ..request
        })
        .unwrap();
    assert_eq!((read.serial, read.boundary, read.mutation), (2, 1, 0));
    let (message, file) = carrier.export().unwrap();
    let (memory, vring) = super::super::tests::queue();
    let mem = memory.memory();
    for (head, addr, length, flags, next) in [
        (0u64, 0x4000u64, 16u32, 1u16, 1u16),
        (1, 0x5000, BLOCK_SIZE as u32, 3, 2),
        (2, 0x6000, 1, 2, 0),
    ] {
        let base = 0x1000 + head * 16;
        mem.write_obj(addr.to_le(), GuestAddress(base)).unwrap();
        mem.write_obj(length.to_le(), GuestAddress(base + 8))
            .unwrap();
        mem.write_obj(flags.to_le(), GuestAddress(base + 12))
            .unwrap();
        mem.write_obj(next.to_le(), GuestAddress(base + 14))
            .unwrap();
    }
    // IN has type zero, sector zero. Avail slot 1 is deliberately poisoned:
    // replay must walk saved head 0, never a consumed available-ring slot.
    mem.write_obj(0xffu8, GuestAddress(0x6000)).unwrap();
    mem.write_obj(2u16, GuestAddress(0x2002)).unwrap();
    mem.write_obj(127u16, GuestAddress(0x2006)).unwrap();
    mem.write_obj(1u16, GuestAddress(0x3002)).unwrap();
    let mut backend =
        Backend::open_with_recovery(&path, BackendKind::LocalAsync, None, true, Fault::default())
            .unwrap();
    backend.update_memory(memory.clone()).unwrap();
    backend.negotiated_features = REQUIRED_FEATURES;
    backend.restore_attachment(&message, file).unwrap();
    assert!(
        backend
            .activate_attachment(&mem, std::slice::from_ref(&vring))
            .unwrap()
    );
    assert_eq!(backend.pending_count(), 1);
    assert_eq!(mem.read_obj::<u8>(GuestAddress(0x6000)).unwrap(), 0xff);
    let deadline = Instant::now() + Duration::from_secs(2);
    while backend.pending_count() != 0 {
        backend
            .complete(&mem, std::slice::from_ref(&vring))
            .unwrap();
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let mut bytes = [0; BLOCK_SIZE];
    mem.read_slice(&mut bytes, GuestAddress(0x5000)).unwrap();
    assert_eq!(bytes, [0x71; BLOCK_SIZE]);
    assert_eq!(mem.read_obj::<u16>(GuestAddress(0x3002)).unwrap(), 2);
    assert_eq!(mem.read_obj::<u32>(GuestAddress(0x300c)).unwrap(), 0);
    backend.drain().unwrap();
    let report = backend.report(0, true);
    assert_eq!(report["reads"], 1);
    assert_eq!(report["local"]["metrics"]["io_queued"], 1);
    assert_eq!(report["local"]["metrics"]["io_completed"], 1);
    assert_eq!(report["local"]["read"]["current"]["bytes"], 0);
    assert!(backend.recovery_deadline.is_none());
}
