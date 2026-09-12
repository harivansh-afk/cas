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
fn expired_admission_wins_over_late_credits_and_control_keeps_its_reserve() {
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
    let flush = Request::Flush(Completion {
        head: 4,
        status: GuestAddress(0x7000),
    });
    assert!(matches!(
        backend.prepare_admission(1, 0, &flush, 136).unwrap(),
        Admission::Accepted(_)
    ));
    backend.waiting[0].as_mut().unwrap().deadline = Instant::now() - Duration::from_nanos(1);
    drop(permits);
    assert!(matches!(
        backend
            .prepare_admission(0, 0, &write_request(), 136)
            .unwrap(),
        Admission::Rejected
    ));
    assert_eq!(
        backend.report(0, true)["local"]["requests"]["admitted"],
        128
    );
    backend.clear_changed_waits(StateChange::QueueStop(0));
    assert!(matches!(
        backend
            .prepare_admission(0, 0, &write_request(), 136)
            .unwrap(),
        Admission::Accepted(_)
    ));
    assert!(backend.failure.is_none());
}

#[test]
fn five_second_timer_rejects_before_mutation_and_later_io_keeps_dense_ids() {
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
    let started = Instant::now();
    backend
        .handle_event(0, EventSet::IN, std::slice::from_ref(&vring), 0)
        .unwrap();
    assert_eq!(backend.next_id, 0);
    assert_eq!(vring.queue_next_avail(), 0);
    assert_eq!(mem.read_obj::<u8>(GuestAddress(0x6000)).unwrap(), 0xff);
    let (fd, token) = backend.deadline_listener().unwrap();
    Deadline::after(Duration::from_secs(7))
        .wait_readable(fd)
        .unwrap();
    backend
        .handle_event(token, EventSet::IN, std::slice::from_ref(&vring), 0)
        .unwrap();
    assert!(started.elapsed() >= ADMISSION_TIMEOUT);
    assert_eq!(
        mem.read_obj::<u8>(GuestAddress(0x6000)).unwrap(),
        Status::IoError as u8
    );
    assert_eq!(mem.read_obj::<u16>(GuestAddress(0x3002)).unwrap(), 1);
    assert!(backend.failure.is_none());
    {
        let mut health = shared.health.lock().unwrap();
        let replay = health
            .carrier
            .as_mut()
            .unwrap()
            .reconcile(&[Some(1)])
            .unwrap();
        assert_eq!(
            (
                replay.highest_serial,
                replay.highest_mutation,
                replay.published
            ),
            (1, 0, 0)
        );
    }
    assert_eq!(
        backend.report(0, true)["local"]["metrics"]["gather_calls"],
        0
    );
    drop(permits);
    // Reuse the head for a new WRITE, followed by READ. The failed request did
    // not spend mutation 1, and the image remains usable without recovery.
    for (index, kind) in [(2, VIRTIO_BLK_T_OUT), (3, VIRTIO_BLK_T_IN)] {
        crate::backend::tests::data_chain(&mem, kind);
        if kind == VIRTIO_BLK_T_IN {
            mem.write_slice(&[0; BLOCK_SIZE], GuestAddress(0x5000))
                .unwrap();
        }
        mem.write_obj(index as u16, GuestAddress(0x2002)).unwrap();
        backend
            .handle_event(0, EventSet::IN, std::slice::from_ref(&vring), 0)
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
        .reconcile(&[Some(3)])
        .unwrap();
    assert_eq!(
        (
            replay.highest_serial,
            replay.highest_mutation,
            replay.published
        ),
        (3, 1, 1)
    );
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
