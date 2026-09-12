//! A real retained carrier and original virtqueue descriptor chain.
use super::*;
use vm_memory::GuestAddress;

pub(crate) struct Frontend {
    pub(crate) backend: Backend,
    memory: GuestMemoryAtomic<GuestMemoryMmap>,
    vrings: [VringMutex; 2],
    pub(crate) carrier: Carrier,
}
enum Pending {
    Write(u8),
    Zero { kind: u32, flags: u32 },
}
impl Frontend {
    pub(crate) fn write(backend: Backend, byte: u8) -> Self {
        Self::new(backend, Pending::Write(byte))
    }
    pub(crate) fn zero_after_write(backend: Backend, kind: u32, flags: u32) -> Self {
        Self::new(backend, Pending::Zero { kind, flags })
    }
    fn new(mut backend: Backend, pending: Pending) -> Self {
        let Storage::Opening(opening) = &backend.storage else {
            panic!("expected retained opening")
        };
        let prefix = opening.status.published;
        let mut carrier = Carrier::create(
            Geometry::new(2, QUEUE_SIZE as u16).unwrap(),
            Identity {
                store: opening.config.store,
                image: opening.config.image,
                epoch: opening.status.epoch,
                attachment: opening.status.epoch,
            },
            opening.config.image_bytes,
            0,
            crate::local::metadata_budget(),
        )
        .unwrap();
        carrier.initialize_queue(0, 0, 0).unwrap();
        let (memory, vring) = super::super::tests::queue();
        let unused = VringMutex::new(memory.clone(), QUEUE_SIZE as u16).unwrap();
        let mem = memory.memory();
        super::super::tests::data_chain(
            &mem,
            virtio_bindings::bindings::virtio_blk::VIRTIO_BLK_T_OUT,
        );
        let byte = match pending {
            Pending::Write(byte) => byte,
            Pending::Zero { .. } => 0x55,
        };
        mem.write_slice(&[byte; BLOCK_SIZE], GuestAddress(0x5000))
            .unwrap();
        let chain = virtio_queue::DescriptorChain::new(
            mem.clone(),
            GuestAddress(0x1000),
            QUEUE_SIZE as u16,
            0,
        );
        let request = decode_chain(&mem, chain, backend.capacity_bytes, &backend.metadata).unwrap();
        let first = carrier.admit(request.inflight(0, 0)).unwrap();
        if let Pending::Zero { kind, flags } = pending {
            // Reconstruct the completed WRITE's carrier history, matching the
            // actual seeded WAL identity before admitting a pending ZERO.
            assert_eq!(prefix, 1);
            carrier.publish(1).unwrap();
            carrier.complete(first, 0, || Ok(())).unwrap();
            mem.write_obj(1u32.to_le(), GuestAddress(0x3008)).unwrap();
            mem.write_obj(1u16.to_le(), GuestAddress(0x3002)).unwrap();
            vring.get_mut().get_queue_mut().set_next_avail(1);
            super::super::tests::data_chain(&mem, kind);
            mem.write_obj(16u32.to_le(), GuestAddress(0x1018)).unwrap();
            let mut range = [0; 16];
            range[8..12].copy_from_slice(&8u32.to_le_bytes());
            range[12..].copy_from_slice(&flags.to_le_bytes());
            mem.write_slice(&range, GuestAddress(0x5000)).unwrap();
            let chain = virtio_queue::DescriptorChain::new(
                mem.clone(),
                GuestAddress(0x1000),
                QUEUE_SIZE as u16,
                0,
            );
            let request =
                decode_chain(&mem, chain, backend.capacity_bytes, &backend.metadata).unwrap();
            carrier.admit(request.inflight(0, 1)).unwrap();
        } else {
            assert_eq!(prefix, 0);
        }
        mem.write_obj(carrier.available(0).unwrap().to_le(), GuestAddress(0x2002))
            .unwrap();
        let (message, file) = carrier.export().unwrap();
        assert!(backend.create_attachment(&message).is_err());
        backend.restore_attachment(&message, file).unwrap();
        backend.update_memory(memory.clone()).unwrap();
        backend.acked_features(backend.features());
        backend.blocked_queues[0] = false;
        Self {
            backend,
            memory,
            vrings: [vring, unused],
            carrier,
        }
    }
    pub(crate) fn activate(&mut self) -> io::Result<bool> {
        self.backend
            .activate_attachment(&self.memory.memory(), &self.vrings)
    }
    pub(crate) fn enable_unused_queue(&mut self) {
        use vhost_user_backend::StateChange;
        let change = StateChange::QueueConfiguration(1);
        self.backend.begin_change(change, &self.vrings).unwrap();
        self.vrings[1].set_queue_size(QUEUE_SIZE as u16);
        self.vrings[1]
            .set_queue_info(0x7000, 0x8000, 0x9000)
            .unwrap();
        self.vrings[1].set_queue_ready(true);
        self.backend.end_change(change, true, &self.vrings).unwrap();
        let change = StateChange::QueueEnable {
            index: 1,
            enabled: true,
        };
        self.backend.begin_change(change, &self.vrings).unwrap();
        self.vrings[1].set_enabled(true);
        self.backend.end_change(change, true, &self.vrings).unwrap();
    }
    pub(crate) fn change_captured_queue(&mut self) -> io::Result<()> {
        self.backend.begin_change(
            vhost_user_backend::StateChange::QueueConfiguration(0),
            &self.vrings,
        )
    }
    pub(crate) fn status(&self) -> u8 {
        self.memory.memory().read_obj(GuestAddress(0x6000)).unwrap()
    }
    pub(crate) fn used(&self) -> u16 {
        self.memory
            .memory()
            .read_obj::<u16>(GuestAddress(0x3002))
            .unwrap()
            .to_le()
    }
    pub(crate) fn local(&mut self) -> &mut local::Local {
        let Storage::Local(local) = &mut self.backend.storage else {
            panic!("expected recovered local")
        };
        local
    }
}
