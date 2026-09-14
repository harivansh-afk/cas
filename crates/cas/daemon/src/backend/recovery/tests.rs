use super::*;
use crate::deadline::Deadline;
use std::time::{Duration, Instant};
use vm_memory::GuestAddress;

#[test]
fn fresh_queue_enable_waits_for_addresses_and_kick() {
    use vhost_user_backend::StateChange;
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
    let (memory, vring) = super::super::tests::queue();
    vring.set_queue_info(0x10000, 0x20000, 0x30000).unwrap();
    vring.set_queue_ready(false);
    vring.set_enabled(false);
    backend.update_memory(memory).unwrap();
    backend.acked_features(backend.features());
    let vrings = [vring];
    let enable = StateChange::QueueEnable {
        index: 0,
        enabled: true,
    };
    backend.begin_change(enable, &vrings).unwrap();
    vrings[0].set_enabled(true);
    backend.end_change(enable, true, &vrings).unwrap();
    assert!(backend.blocked_queues[0]);
    backend
        .begin_change(StateChange::QueueConfiguration(0), &vrings)
        .unwrap();
    vrings[0].set_queue_info(0x1000, 0x2000, 0x3000).unwrap();
    backend
        .end_change(StateChange::QueueConfiguration(0), true, &vrings)
        .unwrap();
    assert!(backend.blocked_queues[0]);
    backend
        .begin_change(StateChange::QueueNotification(0), &vrings)
        .unwrap();
    vrings[0].set_queue_ready(true);
    backend
        .end_change(StateChange::QueueNotification(0), true, &vrings)
        .unwrap();
    assert!(!backend.blocked_queues[0]);
    backend.process(&vrings).unwrap();
    assert!(
        backend
            .storage
            .completion_gate()
            .unwrap()
            .lock()
            .unwrap()
            .carrier
            .as_ref()
            .unwrap()
            .queue_initialized(0)
            .unwrap()
    );
}

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
    backend.deadline_timer = Some(deadline.timer().unwrap());
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
    assert!(
        Carrier::attach(
            file,
            &message,
            carrier.identity(),
            BLOCK_SIZE as u64,
            crate::local::metadata_budget()
        )
        .is_err()
    );
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
        crate::local::metadata_budget(),
    )
    .unwrap();
    carrier.initialize_queue(0, 0, 0).unwrap();
    let request = crate::inflight::Request {
        kind: crate::inflight::Kind::Write,
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
        .admit(crate::inflight::Request {
            kind: crate::inflight::Kind::Read,
            head: 0,
            available: 1,
            ..request
        })
        .unwrap();
    assert_eq!((read.serial, read.boundary, read.mutation), (2, 1, 0));
    let (message, file) = carrier.export().unwrap();
    let (memory, vring) = super::super::tests::queue();
    let mem = memory.memory();
    super::super::tests::data_chain(&mem, virtio_bindings::bindings::virtio_blk::VIRTIO_BLK_T_IN);
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
    let report = serde_json::to_value(backend.report()).unwrap();
    assert_eq!(report["reads"], 1);
    assert_eq!(report["local"]["metrics"]["io_queued"], 1);
    assert_eq!(report["local"]["metrics"]["io_completed"], 1);
    assert_eq!(report["local"]["read"]["current"]["bytes"], 0);
    assert!(backend.recovery_deadline.is_none());
}

#[test]
fn retained_rejected_write_replays_only_ioerr_without_gather_or_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("log");
    let log = local::create_log(&path, 0x10000).unwrap();
    let config = log.config();
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
        crate::local::metadata_budget(),
    )
    .unwrap();
    carrier.initialize_queue(0, 0, 0).unwrap();
    carrier
        .reject(crate::inflight::Request {
            kind: crate::inflight::Kind::Write,
            queue: 0,
            head: 0,
            available: 0,
            offset: 0,
            length: BLOCK_SIZE as u64,
        })
        .unwrap();
    let (message, file) = carrier.export().unwrap();
    let (memory, vring) = crate::backend::tests::queue();
    let mem = memory.memory();
    crate::backend::tests::data_chain(
        &mem,
        virtio_bindings::bindings::virtio_blk::VIRTIO_BLK_T_OUT,
    );
    mem.write_slice(&[0xdd; BLOCK_SIZE], GuestAddress(0x5000))
        .unwrap();
    mem.write_obj(1u16, GuestAddress(0x2002)).unwrap();
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
    assert_eq!(
        mem.read_obj::<u8>(GuestAddress(0x6000)).unwrap(),
        Status::IoError as u8
    );
    assert_eq!(mem.read_obj::<u16>(GuestAddress(0x3002)).unwrap(), 1);
    assert_eq!(backend.next_id, 1);
    assert_eq!(backend.pending_count(), 0);
    assert!(backend.failure.is_none());
    backend.drain().unwrap();
    let report = serde_json::to_value(backend.report()).unwrap();
    assert_eq!(report["writes"], 0);
    assert_eq!(report["local"]["status"]["published"], 0);
    assert_eq!(report["inflight"]["replayed_mutations"], 0);
    assert_eq!(report["inflight"]["replay_copy_bytes"], 0);
    assert_eq!(report["inflight"]["replayed_write_bytes"], 0);
}
