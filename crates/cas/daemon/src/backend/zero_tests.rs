use super::*;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use virtio_bindings::bindings::virtio_blk::{
    VIRTIO_BLK_T_DISCARD, VIRTIO_BLK_T_FLUSH, VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT,
    VIRTIO_BLK_T_WRITE_ZEROES,
};

struct Frontend {
    backend: Backend,
    memory: GuestMemoryAtomic<GuestMemoryMmap>,
    queue: VringMutex,
}
impl Frontend {
    fn new(mut backend: Backend) -> Self {
        let (memory, queue) = super::tests::queue();
        backend.update_memory(memory.clone()).unwrap();
        backend.acked_features(backend.features());
        Self {
            backend,
            memory,
            queue,
        }
    }
    fn command(&mut self, kind: u32, payload: &[u8]) -> u8 {
        let memory = self.memory.memory();
        super::tests::data_chain(&memory, kind);
        if payload.is_empty() {
            memory
                .write_obj(2u16.to_le(), vm_memory::GuestAddress(0x100e))
                .unwrap();
        } else {
            memory
                .write_obj(
                    (payload.len() as u32).to_le(),
                    vm_memory::GuestAddress(0x1018),
                )
                .unwrap();
            memory
                .write_slice(payload, vm_memory::GuestAddress(0x5000))
                .unwrap();
        }
        let available = self.queue.queue_next_avail();
        memory
            .write_obj(
                0u16,
                vm_memory::GuestAddress(0x2004 + 2 * u64::from(available % QUEUE_SIZE as u16)),
            )
            .unwrap();
        memory
            .write_obj(
                available.wrapping_add(1).to_le(),
                vm_memory::GuestAddress(0x2002),
            )
            .unwrap();
        let used = memory
            .read_obj::<u16>(vm_memory::GuestAddress(0x3002))
            .unwrap()
            .to_le();
        self.backend
            .handle_event(0, EventSet::IN, std::slice::from_ref(&self.queue), 0)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while memory
            .read_obj::<u16>(vm_memory::GuestAddress(0x3002))
            .unwrap()
            .to_le()
            == used
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
            self.backend
                .handle_event(
                    self.backend.completion_token(),
                    EventSet::IN,
                    std::slice::from_ref(&self.queue),
                    0,
                )
                .unwrap();
        }
        assert_eq!(
            memory
                .read_obj::<u16>(vm_memory::GuestAddress(0x3002))
                .unwrap()
                .to_le(),
            used.wrapping_add(1)
        );
        memory.read_obj(vm_memory::GuestAddress(0x6000)).unwrap()
    }
    fn prefix(&self) -> (u64, u64) {
        let Storage::Local(local) = &self.backend.storage else {
            panic!("expected local")
        };
        (local.status.published, local.status.durable)
    }
    fn read_zero(&mut self) {
        assert_eq!(
            self.command(VIRTIO_BLK_T_IN, &[0xa5; BLOCK_SIZE]),
            Status::Ok as u8
        );
        let mut bytes = [0xff; BLOCK_SIZE];
        self.memory
            .memory()
            .read_slice(&mut bytes, vm_memory::GuestAddress(0x5000))
            .unwrap();
        assert_eq!(bytes, [0; BLOCK_SIZE]);
    }
}
fn range(sectors: u32, flags: u32) -> [u8; 16] {
    let mut range = [0; 16];
    range[8..12].copy_from_slice(&sectors.to_le_bytes());
    range[12..].copy_from_slice(&flags.to_le_bytes());
    range
}

#[test]
fn shared_queues_execute_zero_variants_and_empty_discards_do_not_wedge_flush() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(crate::Resources::default());
    let mut host = local::host::tests::create(root.path(), 1, Arc::clone(&resources));
    let mut frontend = Frontend::new(host.attach([2; 16], Fault::default()).unwrap());
    let offered = (1 << VIRTIO_BLK_F_DISCARD) | (1 << VIRTIO_BLK_F_WRITE_ZEROES);
    assert_eq!(frontend.backend.features() & offered, offered);
    let config = frontend.backend.get_config(36, 24);
    for (offset, expected) in [(0, 2048u32), (4, 1), (8, 8), (12, 2048), (16, 1)] {
        assert_eq!(
            u32::from_le_bytes(config[offset..offset + 4].try_into().unwrap()),
            expected
        );
    }
    assert_eq!(config[20], 1);
    let mut prefix = 0;
    for (kind, flags) in [
        (VIRTIO_BLK_T_DISCARD, 0),
        (VIRTIO_BLK_T_WRITE_ZEROES, 0),
        (VIRTIO_BLK_T_WRITE_ZEROES, 1),
    ] {
        assert_eq!(
            frontend.command(VIRTIO_BLK_T_OUT, &[0x55; BLOCK_SIZE]),
            Status::Ok as u8
        );
        assert_eq!(frontend.command(kind, &range(8, flags)), Status::Ok as u8);
        prefix += 2;
        assert_eq!(frontend.prefix().0, prefix);
        frontend.read_zero();
    }
    for payload in [&range(0, 0)[..], &[]] {
        assert_eq!(
            frontend.command(VIRTIO_BLK_T_DISCARD, payload),
            Status::Ok as u8
        );
        assert_eq!(frontend.command(VIRTIO_BLK_T_FLUSH, &[]), Status::Ok as u8);
        assert_eq!(frontend.prefix(), (prefix, prefix));
    }
    for (kind, flags) in [(VIRTIO_BLK_T_DISCARD, 1), (VIRTIO_BLK_T_WRITE_ZEROES, 2)] {
        assert_eq!(
            frontend.command(kind, &range(8, flags)),
            Status::Unsupported as u8
        );
        assert_eq!(frontend.prefix().0, prefix);
    }
    frontend
        .backend
        .acked_features(frontend.backend.features() & !(1 << VIRTIO_BLK_F_WRITE_ZEROES));
    assert_eq!(
        frontend.command(VIRTIO_BLK_T_WRITE_ZEROES, &range(8, 0)),
        Status::Unsupported as u8
    );
    assert_eq!(frontend.prefix().0, prefix);
    drop(frontend);
    local::host::tests::shutdown(host);
    assert_eq!(resources.metadata.usage().current.bytes, 0);
}

#[test]
fn reference_frontends_keep_zero_features_unadvertised_and_reject_unnegotiated_requests() {
    let directory = tempfile::tempdir().unwrap();
    for kind in [BackendKind::Local, BackendKind::LocalAsync] {
        let path = directory
            .path()
            .join(if matches!(kind, BackendKind::Local) {
                "sync"
            } else {
                "async"
            });
        let backend = Backend::open_with_recovery(
            &path,
            kind,
            Some(BLOCK_SIZE as u64),
            false,
            Fault::default(),
        )
        .unwrap();
        let mut frontend = Frontend::new(backend);
        let features = (1 << VIRTIO_BLK_F_DISCARD) | (1 << VIRTIO_BLK_F_WRITE_ZEROES);
        assert_eq!(frontend.backend.features() & features, 0);
        // Even an unsolicited feature bit cannot grant a capability not offered.
        frontend
            .backend
            .acked_features(frontend.backend.features() | features);
        assert_eq!(
            frontend.command(VIRTIO_BLK_T_DISCARD, &range(8, 0)),
            Status::Unsupported as u8
        );
        assert_eq!(frontend.prefix(), (0, 0));
    }
}
