use super::*;
use std::time::Duration;
use virtio_bindings::bindings::{
    virtio_blk::{VIRTIO_BLK_T_FLUSH, VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT},
    virtio_ring::{VRING_DESC_F_INDIRECT, VRING_DESC_F_NEXT, VRING_DESC_F_WRITE},
};
use vm_memory::GuestAddress;

struct Guest {
    memory: GuestMemoryAtomic<GuestMemoryMmap>,
    vring: VringMutex,
    available: u16,
    descriptors: u64,
    available_address: u64,
}

impl Guest {
    fn new() -> Self {
        let memory = GuestMemoryAtomic::new(
            GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 8 * 1024 * 1024)]).unwrap(),
        );
        let vring = VringMutex::new(memory.clone(), 256).unwrap();
        vring.set_queue_size(256);
        vring.set_queue_info(0x1000, 0x3000, 0x4000).unwrap();
        vring.set_queue_ready(true);
        vring.set_enabled(true);
        Self {
            memory,
            vring,
            available: 0,
            descriptors: 0x1000,
            available_address: 0x3000,
        }
    }

    fn base(head: u16) -> u64 {
        0x10000 + u64::from(head) * 0x4000
    }
    fn submit(&mut self, head: u16, kind: u32, offset: u64, bytes: usize) {
        let mem = self.memory.memory();
        let base = Self::base(head);
        let descriptor = |at, addr: u64, len: u32, flags: u16, next: u16| {
            mem.write_obj(addr, GuestAddress(at)).unwrap();
            mem.write_obj(len, GuestAddress(at + 8)).unwrap();
            mem.write_obj(flags, GuestAddress(at + 12)).unwrap();
            mem.write_obj(next, GuestAddress(at + 14)).unwrap();
        };
        descriptor(
            self.descriptors + u64::from(head) * 16,
            base,
            if kind == VIRTIO_BLK_T_FLUSH { 32 } else { 48 },
            VRING_DESC_F_INDIRECT as u16,
            0,
        );
        descriptor(base, base + 0x100, 16, VRING_DESC_F_NEXT as u16, 1);
        if kind == VIRTIO_BLK_T_FLUSH {
            descriptor(base + 16, base + 0x3100, 1, VRING_DESC_F_WRITE as u16, 0);
        } else {
            descriptor(
                base + 16,
                base + 0x1000,
                bytes as u32,
                (VRING_DESC_F_NEXT
                    | if kind == VIRTIO_BLK_T_IN {
                        VRING_DESC_F_WRITE
                    } else {
                        0
                    }) as u16,
                2,
            );
            descriptor(base + 32, base + 0x3100, 1, VRING_DESC_F_WRITE as u16, 0);
        }
        mem.write_obj(kind, GuestAddress(base + 0x100)).unwrap();
        mem.write_obj(offset / 512, GuestAddress(base + 0x108))
            .unwrap();
        mem.write_slice(&vec![0x5a; bytes], GuestAddress(base + 0x1000))
            .unwrap();
        mem.write_obj(0xffu8, GuestAddress(base + 0x3100)).unwrap();
        mem.write_obj(
            head,
            GuestAddress(self.available_address + 4 + u64::from(self.available % 256) * 2),
        )
        .unwrap();
        self.available = self.available.wrapping_add(1);
        mem.write_obj(self.available, GuestAddress(self.available_address + 2))
            .unwrap();
    }

    fn status(&self, head: u16) -> u8 {
        self.memory
            .memory()
            .read_obj(GuestAddress(Self::base(head) + 0x3100))
            .unwrap()
    }
    fn bytes(&self, head: u16, expected: u8) {
        let mut bytes = [0; BLOCK_SIZE];
        self.memory
            .memory()
            .read_slice(&mut bytes, GuestAddress(Self::base(head) + 0x1000))
            .unwrap();
        assert_eq!(bytes, [expected; BLOCK_SIZE]);
    }
    fn run_until(&self, backend: &mut Backend, head: u16) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.status(head) == 0xff {
            backend.process(std::slice::from_ref(&self.vring)).unwrap();
            assert!(Instant::now() < deadline, "request {head} did not finish");
            std::thread::yield_now();
        }
        assert_eq!(self.status(head), Status::Ok as u8);
    }
}

fn backend(path: &Path, guest: &Guest) -> Backend {
    let mut backend = Backend::open_with_recovery(
        path,
        BackendKind::LocalAsync,
        Some(1024 * 1024),
        true,
        Fault::default(),
    )
    .unwrap();
    backend.update_memory(guest.memory.clone()).unwrap();
    backend.negotiated_features = backend.features();
    backend
        .create_attachment(&crate::inflight::Geometry::new(1, 256).unwrap().message())
        .unwrap();
    backend
        .end_change(
            vhost_user_backend::StateChange::QueueEnable {
                index: 0,
                enabled: true,
            },
            true,
            std::slice::from_ref(&guest.vring),
        )
        .unwrap();
    backend
}

fn hold_writes(backend: &Backend) -> Vec<local::Permit> {
    let Storage::Local(local) = &backend.storage else {
        unreachable!()
    };
    (0..128)
        .map(|_| {
            local
                .shared
                .reserve(local::Kind::Write(BLOCK_SIZE))
                .unwrap()
        })
        .collect()
}

#[test]
fn disjoint_read_finishes_with_all_write_slots_held_and_no_payload_gather() {
    let root = tempfile::tempdir().unwrap();
    let mut guest = Guest::new();
    let mut backend = backend(&root.path().join("log"), &guest);
    let held = hold_writes(&backend);
    let metadata = backend.metadata.usage().current.bytes;
    guest.submit(0, VIRTIO_BLK_T_OUT, 0, BLOCK_SIZE);
    guest.submit(1, VIRTIO_BLK_T_IN, BLOCK_SIZE as u64, BLOCK_SIZE);
    guest.run_until(&mut backend, 1);
    guest.bytes(1, 0);
    assert_eq!(guest.status(0), 0xff);
    assert_eq!(backend.counters.guest_payload_copy_bytes, 0);
    assert_eq!(backend.frontier.as_ref().unwrap().bypassed, 1);
    assert_eq!(backend.metadata.usage().current.bytes, metadata);
    assert_eq!(backend.next_id, 1);
    drop(held);
    guest.run_until(&mut backend, 0);
    assert!(backend.frontier.as_ref().unwrap().is_empty());
    backend.drain().unwrap();
}

#[test]
fn overlapping_read_and_read_after_flush_wait_for_the_older_write() {
    for (read_offset, flush) in [
        (0, false),
        (BLOCK_SIZE as u64, false),
        (2 * BLOCK_SIZE as u64, true),
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut guest = Guest::new();
        let mut backend = backend(&root.path().join("log"), &guest);
        let held = hold_writes(&backend);
        guest.submit(0, VIRTIO_BLK_T_OUT, 0, 2 * BLOCK_SIZE);
        if flush {
            guest.submit(2, VIRTIO_BLK_T_FLUSH, 0, 0);
        }
        guest.submit(1, VIRTIO_BLK_T_IN, read_offset, BLOCK_SIZE);
        backend.process(std::slice::from_ref(&guest.vring)).unwrap();
        assert_eq!(guest.status(1), 0xff);
        assert_eq!(backend.next_id, 0);
        drop(held);
        guest.run_until(&mut backend, 1);
        guest.bytes(1, if flush { 0 } else { 0x5a });
        assert_eq!(guest.status(0), Status::Ok as u8);
        if flush {
            guest.run_until(&mut backend, 2);
        }
        backend.drain().unwrap();
    }
}

#[test]
fn a_stalled_write_survives_ring_slot_reuse_while_reads_continue() {
    let root = tempfile::tempdir().unwrap();
    let mut guest = Guest::new();
    let mut backend = backend(&root.path().join("log"), &guest);
    let held = hold_writes(&backend);
    guest.submit(0, VIRTIO_BLK_T_OUT, 0, BLOCK_SIZE);
    for _ in 0..300 {
        guest.submit(1, VIRTIO_BLK_T_IN, BLOCK_SIZE as u64, BLOCK_SIZE);
        guest.run_until(&mut backend, 1);
        guest.bytes(1, 0);
        assert_eq!(guest.status(0), 0xff);
        assert_eq!(backend.frontier.as_ref().unwrap().queues[0].len(), 1);
    }
    assert_eq!(backend.frontier.as_ref().unwrap().bypassed, 300);
    drop(held);
    guest.run_until(&mut backend, 0);
    guest.submit(1, VIRTIO_BLK_T_IN, 0, BLOCK_SIZE);
    guest.run_until(&mut backend, 1);
    guest.bytes(1, 0x5a);
    backend.drain().unwrap();
}

#[test]
fn zero_discard_partial_overlap_and_cross_queue_barriers_use_discovery_order() {
    use crate::inflight::{Kind, Request as Header};
    let header = |kind, offset, length| Header {
        kind,
        queue: 0,
        head: 0,
        available: 0,
        offset,
        length,
    };
    for kind in [Kind::Write, Kind::Zero, Kind::Discard, Kind::ZeroUnmap] {
        assert!(!conflicts(
            header(kind, 4096, 0),
            header(Kind::Read, 0, 8192)
        ));
        let older = header(kind, 4096, 8192);
        for (offset, length, blocked) in [
            (0, 4096, false),
            (0, 8192, true),
            (4096, 4096, true),
            (8192, 8192, true),
            (12288, 4096, false),
        ] {
            assert_eq!(
                conflicts(older, header(Kind::Read, offset, length)),
                blocked
            );
        }
    }
    let mut frontier = Frontier::new(&local::metadata_budget()).unwrap();
    let completion = Completion {
        head: 0,
        status: GuestAddress(0),
    };
    frontier
        .restore(1, header(Kind::Flush, 0, 0), Request::Flush(completion))
        .unwrap();
    let identity = Header {
        queue: 1,
        ..header(Kind::Read, 0, 4096)
    };
    frontier
        .restore(
            2,
            identity,
            Request::Read(request::DataRequest {
                completion,
                offset: 0,
                len: 4096,
                segments: request::test_segments([]),
            }),
        )
        .unwrap();
    assert!(!frontier.eligible(1, 0));
    assert!(frontier.eligible(0, 0));
}

#[test]
fn full_ring_preserves_an_independent_read_and_refunds_frontend_metadata() {
    let root = tempfile::tempdir().unwrap();
    let mut guest = Guest::new();
    let mut backend = backend(&root.path().join("log"), &guest);
    let metadata = Arc::clone(&backend.metadata);
    let held = hold_writes(&backend);
    for head in 0..255 {
        guest.submit(head, VIRTIO_BLK_T_OUT, 0, BLOCK_SIZE);
    }
    guest.submit(255, VIRTIO_BLK_T_IN, BLOCK_SIZE as u64, BLOCK_SIZE);
    guest.run_until(&mut backend, 255);
    guest.bytes(255, 0);
    assert_eq!(backend.frontier.as_ref().unwrap().queues[0].len(), 255);
    assert!(
        backend
            .frontier
            .as_ref()
            .unwrap()
            .spans
            .usage()
            .current
            .bytes
            <= 256 * 256 * size_of::<Segment>()
    );
    assert_eq!(backend.counters.guest_payload_copy_bytes, 0);
    drop(held);
    guest.run_until(&mut backend, 254);
    backend.drain().unwrap();
    drop(backend);
    assert_eq!(metadata.usage().current.bytes, 0);
}

#[test]
fn deferred_read_keeps_its_trace_identity_across_available_slot_reuse() {
    let root = tempfile::tempdir().unwrap();
    let mut guest = Guest::new();
    let mut backend = backend(&root.path().join("log"), &guest);
    backend.read_trace = Some(Observer::new(&backend.metadata).unwrap());
    let held = hold_writes(&backend);
    guest.submit(0, VIRTIO_BLK_T_OUT, 0, BLOCK_SIZE);
    guest.submit(1, VIRTIO_BLK_T_IN, 0, BLOCK_SIZE);
    for _ in 0..300 {
        guest.submit(2, VIRTIO_BLK_T_IN, BLOCK_SIZE as u64, BLOCK_SIZE);
        guest.run_until(&mut backend, 2);
        assert_eq!(guest.status(1), 0xff);
    }
    drop(held);
    guest.run_until(&mut backend, 1);
    guest.bytes(1, 0x5a);
    let observer = backend.read_trace.as_ref().unwrap();
    assert_eq!(observer.completed_reads, 301);
    assert_eq!(observer.missing_observations, 0);
    assert_eq!(observer.dropped_traces, 0);
    backend.drain().unwrap();
}

#[test]
fn disconnect_recovery_restores_discovered_write_after_a_completed_bypass_read() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("log");
    let mut guest = Guest::new();
    let mut original = backend(&path, &guest);
    let held = hold_writes(&original);
    guest.submit(0, VIRTIO_BLK_T_OUT, 0, BLOCK_SIZE);
    guest.submit(1, VIRTIO_BLK_T_IN, BLOCK_SIZE as u64, BLOCK_SIZE);
    guest.run_until(&mut original, 1);
    let gate = original.storage.completion_gate().unwrap();
    let (message, file) = gate
        .lock()
        .unwrap()
        .carrier
        .as_ref()
        .unwrap()
        .export()
        .unwrap();
    original.drain().unwrap();
    drop((held, original, gate));
    let mut replacement =
        Backend::open_with_recovery(&path, BackendKind::LocalAsync, None, true, Fault::default())
            .unwrap();
    replacement.restore_attachment(&message, file).unwrap();
    replacement.update_memory(guest.memory.clone()).unwrap();
    replacement.negotiated_features = replacement.features();
    replacement.blocked_queues[0] = false;
    guest.run_until(&mut replacement, 0);
    assert_eq!(
        serde_json::to_value(replacement.live.as_ref().unwrap().report()).unwrap()["replayed_mutations"],
        0
    );
    assert_eq!(replacement.counters.writes, 1);
    guest.submit(1, VIRTIO_BLK_T_IN, 0, BLOCK_SIZE);
    guest.run_until(&mut replacement, 1);
    guest.bytes(1, 0x5a);
    replacement.drain().unwrap();
}

#[test]
fn read_credit_release_retries_without_a_new_guest_publication() {
    let root = tempfile::tempdir().unwrap();
    let mut guest = Guest::new();
    let mut backend = backend(&root.path().join("log"), &guest);
    let writes = hold_writes(&backend);
    let Storage::Local(local) = &backend.storage else {
        unreachable!()
    };
    let mut reads = Vec::new();
    while let Some(permit) = local.shared.reserve(local::Kind::Read(BLOCK_SIZE)) {
        reads.push(permit);
    }
    guest.submit(0, VIRTIO_BLK_T_OUT, 0, BLOCK_SIZE);
    guest.submit(1, VIRTIO_BLK_T_IN, BLOCK_SIZE as u64, BLOCK_SIZE);
    backend.process(std::slice::from_ref(&guest.vring)).unwrap();
    assert_eq!(guest.status(1), 0xff);
    let available = guest.available;
    drop(reads);
    guest.run_until(&mut backend, 1);
    assert_eq!(guest.available, available);
    assert_eq!(guest.status(0), 0xff);
    drop(writes);
    guest.run_until(&mut backend, 0);
    backend.drain().unwrap();
}

#[test]
fn shared_fairness_moves_a_waiting_read_to_the_queue_head_without_a_stale_ticket() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(local::host::Resources::default());
    let mut host = local::host::tests::create(root.path(), 1, Arc::clone(&resources));
    let mut guest = Guest::new();
    let mut backend = host.attach([2; 16], Fault::default()).unwrap();
    backend.update_memory(guest.memory.clone()).unwrap();
    backend.negotiated_features = backend.features();
    let writes = hold_writes(&backend);
    let Storage::Local(local) = &backend.storage else {
        unreachable!()
    };
    let mut reads = Vec::new();
    while let Some(permit) = local.shared.reserve(local::Kind::Read(BLOCK_SIZE)) {
        reads.push(permit);
    }
    guest.submit(0, VIRTIO_BLK_T_OUT, 0, BLOCK_SIZE);
    guest.submit(1, VIRTIO_BLK_T_IN, BLOCK_SIZE as u64, BLOCK_SIZE);
    backend.process(std::slice::from_ref(&guest.vring)).unwrap();
    assert_eq!(guest.status(0), 0xff);
    assert_eq!(guest.status(1), 0xff);
    drop(writes);
    guest.run_until(&mut backend, 0);
    assert_eq!(guest.status(1), 0xff);
    drop(reads);
    guest.run_until(&mut backend, 1);
    guest.bytes(1, 0);
    backend.drain().unwrap();
    drop(backend);
    local::host::tests::shutdown(host);
    assert_eq!(resources.metadata.usage().current.bytes, 0);
}

#[test]
fn shared_zero_and_discard_block_only_overlapping_reads() {
    use virtio_bindings::bindings::virtio_blk::{VIRTIO_BLK_T_DISCARD, VIRTIO_BLK_T_WRITE_ZEROES};
    for (kind, flags) in [
        (VIRTIO_BLK_T_DISCARD, 0u32),
        (VIRTIO_BLK_T_WRITE_ZEROES, 0),
        (VIRTIO_BLK_T_WRITE_ZEROES, 1),
    ] {
        let root = tempfile::tempdir().unwrap();
        let resources = Arc::new(local::host::Resources::default());
        let mut host = local::host::tests::create(root.path(), 1, Arc::clone(&resources));
        let mut guest = Guest::new();
        let mut backend = host.attach([2; 16], Fault::default()).unwrap();
        backend.update_memory(guest.memory.clone()).unwrap();
        backend.negotiated_features = backend.features();
        guest.submit(0, VIRTIO_BLK_T_OUT, 0, BLOCK_SIZE);
        guest.run_until(&mut backend, 0);
        let writes = hold_writes(&backend);
        guest.submit(0, kind, 0, 16);
        let mut range = [0; 16];
        range[8..12].copy_from_slice(&8u32.to_le_bytes());
        range[12..].copy_from_slice(&flags.to_le_bytes());
        guest
            .memory
            .memory()
            .write_slice(&range, GuestAddress(Guest::base(0) + 0x1000))
            .unwrap();
        guest.submit(1, VIRTIO_BLK_T_IN, 0, BLOCK_SIZE);
        guest.submit(2, VIRTIO_BLK_T_IN, BLOCK_SIZE as u64, BLOCK_SIZE);
        guest.run_until(&mut backend, 2);
        assert_eq!(guest.status(0), 0xff);
        assert_eq!(guest.status(1), 0xff);
        guest.bytes(2, 0);
        drop(writes);
        guest.run_until(&mut backend, 1);
        guest.bytes(1, 0);
        backend.drain().unwrap();
        drop(backend);
        local::host::tests::shutdown(host);
        assert_eq!(resources.metadata.usage().current.bytes, 0);
    }
}

#[test]
fn configuration_drains_discovered_owners_before_replacing_guest_memory() {
    let root = tempfile::tempdir().unwrap();
    let mut guest = Guest::new();
    let mut backend = backend(&root.path().join("log"), &guest);
    let held = hold_writes(&backend);
    guest.submit(0, VIRTIO_BLK_T_OUT, 0, BLOCK_SIZE);
    guest.submit(1, VIRTIO_BLK_T_IN, BLOCK_SIZE as u64, BLOCK_SIZE);
    guest.run_until(&mut backend, 1);
    assert!(backend.update_memory(guest.memory.clone()).is_err());
    drop(held);
    let change = vhost_user_backend::StateChange::Memory;
    backend
        .begin_change(change, std::slice::from_ref(&guest.vring))
        .unwrap();
    assert_eq!(guest.status(0), Status::Ok as u8);
    assert!(backend.frontier.as_ref().unwrap().is_empty());
    backend.update_memory(guest.memory.clone()).unwrap();
    backend
        .end_change(change, true, std::slice::from_ref(&guest.vring))
        .unwrap();
    guest.submit(1, VIRTIO_BLK_T_IN, 0, BLOCK_SIZE);
    guest.run_until(&mut backend, 1);
    guest.bytes(1, 0x5a);
    backend.drain().unwrap();
}

#[test]
fn cross_queue_reads_wait_for_older_overlaps_but_disjoint_reads_progress() {
    let root = tempfile::tempdir().unwrap();
    let mut writer = Guest::new();
    let vring = VringMutex::new(writer.memory.clone(), 256).unwrap();
    vring.set_queue_size(256);
    vring.set_queue_info(0x5000, 0x6000, 0x7000).unwrap();
    vring.set_queue_ready(true);
    vring.set_enabled(true);
    let mut reader = Guest {
        memory: writer.memory.clone(),
        vring,
        available: 0,
        descriptors: 0x5000,
        available_address: 0x6000,
    };
    let mut backend = Backend::open_with_recovery(
        &root.path().join("log"),
        BackendKind::LocalAsync,
        Some(1024 * 1024),
        false,
        Fault::default(),
    )
    .unwrap();
    backend.update_memory(writer.memory.clone()).unwrap();
    backend.negotiated_features = backend.features();
    let writes = hold_writes(&backend);
    writer.submit(0, VIRTIO_BLK_T_OUT, 0, BLOCK_SIZE);
    backend
        .admit_queue(&writer.memory.memory(), 0, &writer.vring)
        .unwrap();
    reader.submit(1, VIRTIO_BLK_T_IN, 0, BLOCK_SIZE);
    reader.submit(2, VIRTIO_BLK_T_IN, BLOCK_SIZE as u64, BLOCK_SIZE);
    let vrings = [writer.vring.clone(), reader.vring.clone()];
    let deadline = Instant::now() + Duration::from_secs(3);
    while reader.status(2) == 0xff {
        backend.process(&vrings).unwrap();
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(reader.status(2), Status::Ok as u8);
    reader.bytes(2, 0);
    assert_eq!(reader.status(1), 0xff);
    assert_eq!(writer.status(0), 0xff);
    drop(writes);
    while reader.status(1) == 0xff {
        backend.process(&vrings).unwrap();
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(reader.status(1), Status::Ok as u8);
    reader.bytes(1, 0x5a);
    backend.drain().unwrap();
}

#[test]
fn releasing_write_capacity_makes_progress_while_reads_keep_arriving() {
    let root = tempfile::tempdir().unwrap();
    let mut guest = Guest::new();
    let mut backend = backend(&root.path().join("log"), &guest);
    let mut writes = Some(hold_writes(&backend));
    guest.submit(0, VIRTIO_BLK_T_OUT, 0, BLOCK_SIZE);
    for index in 0..100 {
        if index == 20 {
            drop(writes.take());
        }
        guest.submit(1, VIRTIO_BLK_T_IN, BLOCK_SIZE as u64, BLOCK_SIZE);
        guest.run_until(&mut backend, 1);
        if index < 20 {
            assert_eq!(guest.status(0), 0xff);
        }
    }
    assert_eq!(guest.status(0), Status::Ok as u8);
    assert_eq!(backend.counters.reads, 100);
    assert_eq!(backend.counters.writes, 1);
    backend.drain().unwrap();
}

#[test]
fn fresh_attachment_restarts_discovery_identity_and_preserves_image_contents() {
    use vhost_user_backend::StateChange;
    let root = tempfile::tempdir().unwrap();
    let mut guest = Guest::new();
    let mut backend = backend(&root.path().join("log"), &guest);
    guest.submit(0, VIRTIO_BLK_T_OUT, 0, BLOCK_SIZE);
    guest.run_until(&mut backend, 0);
    assert_eq!(backend.frontier.as_ref().unwrap().next_order, 1);
    backend
        .begin_change(StateChange::Attachment, std::slice::from_ref(&guest.vring))
        .unwrap();
    backend
        .create_attachment(&crate::inflight::Geometry::new(1, 256).unwrap().message())
        .unwrap();
    backend
        .end_change(
            StateChange::Attachment,
            true,
            std::slice::from_ref(&guest.vring),
        )
        .unwrap();
    let enable = StateChange::QueueEnable {
        index: 0,
        enabled: true,
    };
    backend
        .begin_change(enable, std::slice::from_ref(&guest.vring))
        .unwrap();
    backend
        .end_change(enable, true, std::slice::from_ref(&guest.vring))
        .unwrap();
    guest.submit(1, VIRTIO_BLK_T_IN, 0, BLOCK_SIZE);
    guest.run_until(&mut backend, 1);
    guest.bytes(1, 0x5a);
    assert_eq!(backend.frontier.as_ref().unwrap().next_order, 1);
    backend.drain().unwrap();
}
