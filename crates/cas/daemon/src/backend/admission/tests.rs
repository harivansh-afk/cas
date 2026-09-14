use super::*;
use crate::inflight::Geometry;
use crate::request::DataRequest;
use virtio_bindings::bindings::virtio_blk::{VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT};
use vm_memory::GuestAddress;

fn write_request() -> Request {
    Request::Write(DataRequest {
        completion: Completion {
            head: 0,
            status: GuestAddress(0x6000),
        },
        offset: 0,
        len: BLOCK_SIZE,
        segments: request::test_segments([Segment {
            addr: GuestAddress(0x5000),
            len: BLOCK_SIZE,
            writable: false,
        }]),
    })
}

fn shared(backend: &Backend) -> cas_core::budget::BudgetArc<local::Shared> {
    let Storage::Local(local) = &backend.storage else {
        panic!("expected local worker")
    };
    local.shared.clone()
}

#[test]
fn read_trace_follows_an_actual_read_from_available_to_used() {
    let directory = tempfile::tempdir().unwrap();
    let mut backend = Backend::open_with_recovery(
        &directory.path().join("log"),
        BackendKind::LocalAsync,
        Some(0x10000),
        false,
        Fault::default(),
    )
    .unwrap();
    let (memory, vring) = crate::backend::tests::queue();
    let mem = memory.memory();
    backend.update_memory(memory.clone()).unwrap();
    backend.negotiated_features = REQUIRED_FEATURES;
    backend.read_trace = Some(crate::read_trace::Observer::new(&backend.metadata).unwrap());
    crate::backend::tests::data_chain(&mem, VIRTIO_BLK_T_IN);
    mem.write_obj(1u16, GuestAddress(0x2002)).unwrap();
    backend.process(std::slice::from_ref(&vring)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while backend.pending_count() != 0 {
        backend
            .complete(&mem, std::slice::from_ref(&vring))
            .unwrap();
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let observer = backend.read_trace.as_ref().unwrap();
    assert_eq!(observer.completed_reads, 1);
    assert_eq!(observer.missing_observations, 0);
    assert_eq!(observer.dropped_traces, 0);
    let trace = observer.slowest.iter().flatten().next().unwrap();
    assert!(trace.success);
    assert_eq!((trace.queue, trace.offset, trace.bytes), (0, 0, 4096));
    let milestones = [
        trace.head_ns,
        trace.admitted_ns,
        trace.enqueued_ns,
        trace.received_ns,
        trace.started_ns,
        trace.responded_ns,
        trace.frontend_received_ns,
        trace.finished_ns,
    ];
    assert!(milestones.windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!(vring.queue_next_avail(), 1);
    assert_eq!(mem.read_obj::<u16>(GuestAddress(0x3002)).unwrap(), 1);
    assert_eq!(
        mem.read_obj::<u8>(GuestAddress(0x6000)).unwrap(),
        Status::Ok as u8
    );
    backend.drain().unwrap();
}

#[test]
fn metadata_release_resumes_on_the_admission_timer_without_a_guest_kick() {
    use cas_core::budget::Amount;
    let directory = tempfile::tempdir().unwrap();
    let mut backend = Backend::open_with_recovery(
        &directory.path().join("log"),
        BackendKind::LocalAsync,
        Some(0x10000),
        false,
        Fault::default(),
    )
    .unwrap();
    let (memory, vring) = crate::backend::tests::queue();
    let mem = memory.memory();
    backend.update_memory(memory.clone()).unwrap();
    backend.negotiated_features = REQUIRED_FEATURES;
    let shared = shared(&backend);
    let held = shared
        .metadata()
        .reserve(Amount {
            bytes: 128 * MAX_REQUEST_BYTES - shared.metadata().usage().current.bytes,
            requests: 0,
        })
        .unwrap();
    crate::backend::tests::data_chain(&mem, VIRTIO_BLK_T_IN);
    mem.write_obj(1u16, GuestAddress(0x2002)).unwrap();
    // Model a concurrent metadata claim after descriptor parsing. The initial
    // request already owns its spans; only the read-credit owner is refused.
    let Request::Write(mut data) = write_request() else {
        unreachable!()
    };
    data.segments = request::test_segments([Segment {
        addr: GuestAddress(0x5000),
        len: BLOCK_SIZE,
        writable: true,
    }]);
    assert!(matches!(
        backend
            .prepare_admission(0, 0, &Request::Read(data), 136)
            .unwrap(),
        Admission::Waiting
    ));
    backend.rearm_deadline_timer().unwrap();
    assert_eq!(
        backend.admission_report()["heads"][0]["reason"]["storage"],
        "read_owner_allocation"
    );
    assert_eq!(vring.queue_next_avail(), 0);
    drop(held);
    let (fd, token) = backend.deadline_listener().unwrap();
    Deadline::after(Duration::from_secs(1))
        .wait_readable(fd)
        .unwrap();
    backend
        .handle_event(token, EventSet::IN, std::slice::from_ref(&vring), 0)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while backend.pending_count() != 0 {
        backend
            .complete(&mem, std::slice::from_ref(&vring))
            .unwrap();
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(vring.queue_next_avail(), 1);
    assert_eq!(
        mem.read_obj::<u8>(GuestAddress(0x6000)).unwrap(),
        Status::Ok as u8
    );
    assert_eq!(backend.admission_report()["statistics"]["resumed"], 1);
    assert_eq!(
        io::Error::from(backend.deadline_timer.as_mut().unwrap().wait().unwrap_err()).kind(),
        io::ErrorKind::WouldBlock
    );
    backend.drain().unwrap();
}

#[test]
fn capacity_wait_survives_the_old_deadline_and_control_keeps_its_reserve() {
    let directory = tempfile::tempdir().unwrap();
    let mut backend = Backend::open_with_recovery(
        &directory.path().join("log"),
        BackendKind::LocalAsync,
        Some(0x10000),
        false,
        Fault::default(),
    )
    .unwrap();
    let shared = shared(&backend);
    let permits: Vec<_> = (0..128)
        .map(|_| shared.reserve(local::Kind::Write(BLOCK_SIZE)).unwrap())
        .collect();
    assert!(matches!(
        backend
            .prepare_admission(0, 0, &write_request(), 136)
            .unwrap(),
        Admission::Waiting
    ));
    assert_eq!(
        backend.report(0, true)["local"]["admission_denials"]["request_credits"],
        1
    );
    let flush = Request::Flush(Completion {
        head: 4,
        status: GuestAddress(0x7000),
    });
    assert!(matches!(
        backend.prepare_admission(1, 0, &flush, 136).unwrap(),
        Admission::Accepted(_)
    ));
    backend.admission.heads[0].as_mut().unwrap().started = Instant::now() - Duration::from_secs(6);
    assert!(matches!(
        backend
            .prepare_admission(0, 0, &write_request(), 136)
            .unwrap(),
        Admission::Waiting
    ));
    drop(permits);
    assert!(matches!(
        backend
            .prepare_admission(0, 0, &write_request(), 136)
            .unwrap(),
        Admission::Accepted(_)
    ));
    assert_eq!(
        backend.report(0, true)["local"]["requests"]["admitted"],
        129
    );
    backend.admission.changed(StateChange::QueueStop(0));
    assert!(matches!(
        backend
            .prepare_admission(0, 0, &write_request(), 136)
            .unwrap(),
        Admission::Accepted(_)
    ));
    assert!(backend.failure.is_none());
}

#[test]
fn long_capacity_wait_keeps_the_descriptor_and_resumes_without_an_error_or_id_gap() {
    let directory = tempfile::tempdir().unwrap();
    let mut backend = Backend::open_with_recovery(
        &directory.path().join("log"),
        BackendKind::LocalAsync,
        Some(0x10000),
        true,
        Fault::default(),
    )
    .unwrap();
    let (memory, vring) = crate::backend::tests::queue();
    let mem = memory.memory();
    backend.update_memory(memory.clone()).unwrap();
    backend.negotiated_features = REQUIRED_FEATURES;
    backend
        .create_attachment(&Geometry::new(1, 128).unwrap().message())
        .unwrap();
    backend
        .end_change(
            StateChange::QueueEnable {
                index: 0,
                enabled: true,
            },
            true,
            std::slice::from_ref(&vring),
        )
        .unwrap();
    let shared = shared(&backend);
    let permits: Vec<_> = (0..128)
        .map(|_| shared.reserve(local::Kind::Write(BLOCK_SIZE)).unwrap())
        .collect();
    crate::backend::tests::data_chain(&mem, VIRTIO_BLK_T_OUT);
    mem.write_slice(&[0x61; BLOCK_SIZE], GuestAddress(0x5000))
        .unwrap();
    mem.write_obj(1u16, GuestAddress(0x2002)).unwrap();
    backend
        .handle_event(0, EventSet::IN, std::slice::from_ref(&vring), 0)
        .unwrap();
    assert_eq!(backend.next_id, 0);
    assert_eq!(vring.queue_next_avail(), 0);
    assert_eq!(mem.read_obj::<u8>(GuestAddress(0x6000)).unwrap(), 0xff);
    // Advance the recorded wait age deterministically; no sleep or guest timer.
    backend.admission.heads[0].as_mut().unwrap().started = Instant::now() - Duration::from_secs(6);
    backend
        .handle_event(
            backend.completion_token(),
            EventSet::IN,
            std::slice::from_ref(&vring),
            0,
        )
        .unwrap();
    assert_eq!(mem.read_obj::<u8>(GuestAddress(0x6000)).unwrap(), 0xff);
    assert_eq!(mem.read_obj::<u16>(GuestAddress(0x3002)).unwrap(), 0);
    assert_eq!(vring.queue_next_avail(), 0);
    assert_eq!(backend.next_id, 0);
    let report = backend.admission_report();
    assert_eq!(report["heads"][0]["reason"]["storage"], "request_credits");
    assert!(report["heads"][0]["wait_ns"].as_u64().unwrap() >= 6_000_000_000);
    assert_eq!(report["statistics"]["started"], 1);
    assert!(backend.failure.is_none());
    {
        let mut health = shared.health.lock().unwrap();
        let replay = health
            .carrier
            .as_mut()
            .unwrap()
            .reconcile(&[Some(0)])
            .unwrap();
        assert_eq!(
            (
                replay.highest_serial,
                replay.highest_mutation,
                replay.published
            ),
            (0, 0, 0)
        );
    }
    assert_eq!(
        backend.report(0, true)["local"]["metrics"]["gather_calls"],
        0
    );
    drop(permits);
    // Resume the original WRITE on a capacity notification, then reuse its head
    // for a READ. Waiting never created a rejected carrier entry or mutation.
    for (index, kind) in [(1, VIRTIO_BLK_T_OUT), (2, VIRTIO_BLK_T_IN)] {
        crate::backend::tests::data_chain(&mem, kind);
        if kind == VIRTIO_BLK_T_IN {
            mem.write_slice(&[0; BLOCK_SIZE], GuestAddress(0x5000))
                .unwrap();
        }
        mem.write_obj(index as u16, GuestAddress(0x2002)).unwrap();
        backend
            .handle_event(
                backend.completion_token(),
                EventSet::IN,
                std::slice::from_ref(&vring),
                0,
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while backend.pending_count() != 0 {
            backend
                .complete(&mem, std::slice::from_ref(&vring))
                .unwrap();
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(
            mem.read_obj::<u8>(GuestAddress(0x6000)).unwrap(),
            Status::Ok as u8
        );
    }
    let mut bytes = [0; BLOCK_SIZE];
    mem.read_slice(&mut bytes, GuestAddress(0x5000)).unwrap();
    assert_eq!(bytes, [0x61; BLOCK_SIZE]);
    let replay = shared
        .health
        .lock()
        .unwrap()
        .carrier
        .as_mut()
        .unwrap()
        .reconcile(&[Some(2)])
        .unwrap();
    assert_eq!(
        (
            replay.highest_serial,
            replay.highest_mutation,
            replay.published
        ),
        (2, 1, 1)
    );
    assert_eq!(backend.admission_report()["statistics"]["resumed"], 1);
    assert_eq!(backend.admission_report()["statistics"]["canceled"], 0);
    assert_eq!(backend.admission_report()["heads"], serde_json::json!([]));
    backend.drain().unwrap();
}

#[test]
fn rejected_status_cannot_be_published_as_success() {
    use crate::inflight::{Carrier, Identity};
    let (memory, vring) = crate::backend::tests::queue();
    let mem = memory.memory();
    let request = write_request();
    mem.write_obj(0xffu8, request.completion().status).unwrap();
    let mut carrier = Carrier::create(
        Geometry::new(1, 128).unwrap(),
        Identity {
            store: [1; 16],
            image: [2; 16],
            epoch: 1,
            attachment: 1,
        },
        0x10000,
        0,
        crate::local::metadata_budget(),
    )
    .unwrap();
    carrier.initialize_queue(0, 0, 0).unwrap();
    let entry = carrier.reject(request.inflight(0, 0)).unwrap();
    let mut health = local::ImageState {
        carrier: Some(carrier),
        ..Default::default()
    };
    assert!(
        publish_tracked(
            &mem,
            &mut vring.get_mut(),
            GuestCompletion {
                queue: 0,
                target: request.completion(),
                inflight: Some(entry),
                write_number: None,
            },
            Status::Ok,
            None,
            &mut Fault::default(),
            Some(&mut health)
        )
        .is_err()
    );
    assert!(health.failure.is_some());
    assert_eq!(
        mem.read_obj::<u8>(request.completion().status).unwrap(),
        0xff
    );
    assert_eq!(mem.read_obj::<u16>(GuestAddress(0x3002)).unwrap(), 0);
}

#[test]
fn shared_frontends_defer_full_images_keep_control_live_and_wake_on_credit_release() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(local::host::Resources::default());
    let mut host = local::host::tests::create(root.path(), 2, Arc::clone(&resources));
    let mut first = host.attach([2; 16], Fault::default()).unwrap();
    let mut second = host.attach([3; 16], Fault::default()).unwrap();
    let mut held = Vec::new();
    for available in 0..128 {
        let Admission::Accepted(permit) = first
            .prepare_admission(0, available, &write_request(), 136)
            .unwrap()
        else {
            panic!("first image request capacity");
        };
        first.admission.heads[0] = None;
        held.push(permit);
    }
    assert!(matches!(
        first
            .prepare_admission(0, 128, &write_request(), 136)
            .unwrap(),
        Admission::Waiting
    ));
    let Admission::Accepted(other) = second
        .prepare_admission(0, 0, &write_request(), 136)
        .unwrap()
    else {
        panic!("full first image blocked second image");
    };
    second.admission.heads[0] = None;
    let flush = Request::Flush(Completion {
        head: 4,
        status: GuestAddress(0x7000),
    });
    let Admission::Accepted(control) = first.prepare_admission(1, 0, &flush, 136).unwrap() else {
        panic!("bulk pressure consumed the control reserve");
    };
    first.admission.heads[1] = None;
    assert_eq!((first.next_id, second.next_id), (0, 0));
    // Drain earlier readiness, then release one actual accepted request owner.
    while first.completion_event.read().is_ok() {}
    drop(held.pop());
    assert!(first.completion_event.read().is_ok());
    let Admission::Accepted(retry) = first
        .prepare_admission(0, 128, &write_request(), 136)
        .unwrap()
    else {
        panic!("released credit did not admit the waiting head");
    };
    first.admission.heads[0] = None;
    drop((retry, control, other, held, first, second));
    local::host::tests::shutdown(host);
    assert_eq!(resources.metadata.usage().current.bytes, 0);
}

#[test]
fn queue_changes_and_failure_cancel_waiting_tickets_without_consuming_guest_ids() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(local::host::Resources::default());
    let mut host = local::host::tests::create(root.path(), 1, Arc::clone(&resources));
    let mut backend = host.attach([2; 16], Fault::default()).unwrap();
    let shared = shared(&backend);
    let held: Vec<_> = (0..128)
        .map(|_| shared.reserve(local::Kind::Write(BLOCK_SIZE)).unwrap())
        .collect();
    let changes = [
        StateChange::Memory,
        StateChange::Reset,
        StateChange::Attachment,
        StateChange::QueueConfiguration(0),
        StateChange::QueueStop(0),
        StateChange::QueueEnable {
            index: 0,
            enabled: false,
        },
    ];
    for (index, change) in changes.into_iter().enumerate() {
        assert!(matches!(
            backend
                .prepare_admission(0, index as u16, &write_request(), 136)
                .unwrap(),
            Admission::Waiting
        ));
        backend.admission.changed(change);
        assert!(backend.admission.heads.iter().all(Option::is_none));
        assert_eq!(
            backend.admission_report()["statistics"]["canceled"],
            index + 1
        );
    }
    assert!(matches!(
        backend
            .prepare_admission(0, 10, &write_request(), 136)
            .unwrap(),
        Admission::Waiting
    ));
    backend.fail("injected storage failure".into());
    assert!(backend.admission.heads.iter().all(Option::is_none));
    assert_eq!(backend.admission_report()["statistics"]["canceled"], 7);
    assert_eq!(backend.admission_report()["statistics"]["resumed"], 0);
    assert_eq!(backend.next_id, 0);
    drop((held, shared, backend));
    local::host::tests::shutdown(host);
    assert_eq!(resources.metadata.usage().current.bytes, 0);
}

#[test]
fn another_virtqueue_read_passes_a_write_waiting_for_wal_capacity() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(local::host::Resources::default());
    let mut host = local::host::tests::create(root.path(), 1, Arc::clone(&resources));
    let mut backend = host.attach([2; 16], Fault::default()).unwrap();
    let Request::Write(mut data) = write_request() else {
        unreachable!()
    };
    data.len = 64 * 1024;
    data.segments = request::test_segments([Segment {
        addr: GuestAddress(0x5000),
        len: data.len,
        writable: false,
    }]);
    let write = Request::Write(data);
    let mut held = Vec::new();
    while let Admission::Accepted(permit) = backend
        .prepare_admission(0, held.len() as u16, &write, 136)
        .unwrap()
    {
        backend.admission.finish_wait(0, true);
        held.push(permit);
        assert!(
            held.len() < 128,
            "expected byte pressure before request pressure"
        );
    }
    assert_eq!(
        backend.admission_report()["heads"][0]["reason"]["storage"],
        "wal_rotation"
    );
    let Request::Write(mut data) = write_request() else {
        unreachable!()
    };
    data.completion.head = 3;
    data.segments = request::test_segments([Segment {
        addr: GuestAddress(0x5000),
        len: BLOCK_SIZE,
        writable: true,
    }]);
    let Admission::Accepted(read) = backend
        .prepare_admission(1, 0, &Request::Read(data), 136)
        .unwrap()
    else {
        panic!("blocked write prevented an eligible read on another queue");
    };
    backend.admission.finish_wait(1, true);
    assert_eq!(backend.next_id, 0); // Reservation has not assigned a guest serial.
    drop((read, held, backend));
    local::host::tests::shutdown(host);
    assert_eq!(resources.metadata.usage().current.bytes, 0);
}
