//! Four real split rings on one socket, including incomplete reconnect setup.
use super::*;

const QUEUES: usize = 4;
const SIZE: u16 = 256;
const STRIDE: u64 = 0x10000;

struct Ring {
    call: EventFd,
    kick: EventFd,
    available: u16,
}

struct Queues {
    frontend: Frontend,
    mem: GuestMemoryMmap,
    rings: [Ring; QUEUES],
    inflight: Option<(VhostUserInflight, File)>,
}

impl Queues {
    fn connect(daemon: &mut Daemon, start: u16) -> Self {
        let file = tempfile::tempfile_in(daemon.directory.path()).unwrap();
        file.set_len(STRIDE * QUEUES as u64).unwrap();
        let mem = GuestMemoryMmap::from_ranges_with_files([(
            GuestAddress(0),
            (STRIDE * QUEUES as u64) as usize,
            Some(FileOffset::new(file, 0)),
        )])
        .unwrap();
        let mut queues = Self {
            frontend: Frontend::from_stream(daemon.connect(), QUEUES as u64),
            mem,
            rings: std::array::from_fn(|_| Ring {
                call: EventFd::new(EFD_NONBLOCK | EFD_CLOEXEC).unwrap(),
                kick: EventFd::new(EFD_NONBLOCK | EFD_CLOEXEC).unwrap(),
                available: start,
            }),
            inflight: None,
        };
        queues.negotiate();
        for index in 0..QUEUES {
            queues
                .mem
                .write_obj(start.to_le(), GuestAddress(index as u64 * STRIDE + 0x3002))
                .unwrap();
            queues
                .mem
                .write_obj(start.to_le(), GuestAddress(index as u64 * STRIDE + 0x2002))
                .unwrap();
            queues.configure(index, start);
        }
        queues
    }

    fn negotiate(&mut self) {
        self.frontend.set_owner().unwrap();
        let features = self.frontend.get_features().unwrap();
        self.frontend.set_features(features).unwrap();
        let protocol = self.frontend.get_protocol_features().unwrap();
        assert!(protocol.contains(VhostUserProtocolFeatures::MQ));
        self.frontend.set_protocol_features(protocol).unwrap();
        assert_eq!(self.frontend.get_queue_num().unwrap(), QUEUES as u64);
        if self.inflight.is_none() {
            self.inflight = Some(
                self.frontend
                    .get_inflight_fd(&VhostUserInflight {
                        mmap_size: 0,
                        mmap_offset: 0,
                        num_queues: QUEUES as u16,
                        queue_size: SIZE,
                    })
                    .unwrap(),
            );
        }
        let (message, file) = self.inflight.as_ref().unwrap();
        self.frontend
            .set_inflight_fd(message, file.as_raw_fd())
            .unwrap();
        let region =
            VhostUserMemoryRegionInfo::from_guest_region(self.mem.iter().next().unwrap()).unwrap();
        self.frontend.set_mem_table(&[region]).unwrap();
    }

    fn configure(&mut self, index: usize, base: u16) {
        let region =
            VhostUserMemoryRegionInfo::from_guest_region(self.mem.iter().next().unwrap()).unwrap();
        let address = region.userspace_addr + index as u64 * STRIDE;
        self.frontend.set_vring_num(index, SIZE).unwrap();
        self.frontend
            .set_vring_addr(
                index,
                &VringConfigData {
                    queue_max_size: SIZE,
                    queue_size: SIZE,
                    desc_table_addr: address + 0x1000,
                    avail_ring_addr: address + 0x2000,
                    used_ring_addr: address + 0x3000,
                    ..Default::default()
                },
            )
            .unwrap();
        self.frontend.set_vring_base(index, base).unwrap();
        self.frontend
            .set_vring_call(index, &self.rings[index].call)
            .unwrap();
        self.frontend
            .set_vring_kick(index, &self.rings[index].kick)
            .unwrap();
        self.frontend.set_vring_enable(index, true).unwrap();
        self.frontend.get_features().unwrap();
    }

    fn submit(&mut self, index: usize, kind: u32, value: u8) {
        let base = index as u64 * STRIDE;
        self.mem
            .write_obj(kind.to_le(), GuestAddress(base + 0x4000))
            .unwrap();
        self.mem
            .write_slice(&[value; BLOCK_SIZE], GuestAddress(base + 0x5000))
            .unwrap();
        self.mem
            .write_obj(0xffu8, GuestAddress(base + 0x6000))
            .unwrap();
        let mut descriptors = vec![(base + 0x4000, 16, VRING_DESC_F_NEXT)];
        if kind != VIRTIO_BLK_T_FLUSH {
            descriptors.push((
                base + 0x5000,
                BLOCK_SIZE as u32,
                VRING_DESC_F_NEXT
                    | if kind == VIRTIO_BLK_T_IN {
                        VRING_DESC_F_WRITE
                    } else {
                        0
                    },
            ));
        }
        descriptors.push((base + 0x6000, 1, VRING_DESC_F_WRITE));
        for (head, (address, length, flags)) in descriptors.iter().enumerate() {
            let mut bytes = [0; 16];
            bytes[..8].copy_from_slice(&address.to_le_bytes());
            bytes[8..12].copy_from_slice(&length.to_le_bytes());
            bytes[12..14].copy_from_slice(&(*flags as u16).to_le_bytes());
            bytes[14..].copy_from_slice(&((head + 1) as u16).to_le_bytes());
            self.mem
                .write_slice(&bytes, GuestAddress(base + 0x1000 + head as u64 * 16))
                .unwrap();
        }
        let ring = &mut self.rings[index];
        self.mem
            .write_obj(
                0u16,
                GuestAddress(base + 0x2004 + u64::from(ring.available % SIZE) * 2),
            )
            .unwrap();
        ring.available = ring.available.wrapping_add(1);
        self.mem
            .write_obj(ring.available.to_le(), GuestAddress(base + 0x2002))
            .unwrap();
        ring.kick.write(1).unwrap();
    }

    fn used(&self, index: usize) -> u16 {
        u16::from_le(
            self.mem
                .read_obj(GuestAddress(index as u64 * STRIDE + 0x3002))
                .unwrap(),
        )
    }

    fn wait(&self, index: usize, daemon: &Daemon) {
        while self.used(index) != self.rings[index].available {
            assert!(
                Instant::now() < daemon.deadline,
                "queue {index}: {}",
                daemon.stderr()
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            self.mem
                .read_obj::<u8>(GuestAddress(index as u64 * STRIDE + 0x6000))
                .unwrap(),
            VIRTIO_BLK_S_OK as u8
        );
    }
}

#[test]
fn replay_waits_for_all_old_queues_and_preserves_global_write_order() {
    for point in [
        "before-submit",
        "after-storage",
        "after-status",
        "after-used",
    ] {
        for start in [0, u16::MAX - 1] {
            let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
            let child = Daemon::start_kind(&directory, "local-async", true, true, Some(point));
            let mut daemon = Daemon {
                child,
                directory,
                deadline: Instant::now() + DEADLINE,
            };
            let mut queues = Queues::connect(&mut daemon, start);
            for index in 0..QUEUES {
                queues.submit(index, VIRTIO_BLK_T_IN, 0);
                queues.wait(index, &daemon); // Establish all four retained queue identities.
            }
            queues.submit(0, VIRTIO_BLK_T_OUT, 0x11);
            queues.wait(0, &daemon);
            queues.submit(1, VIRTIO_BLK_T_OUT, 0x22);
            let marker = daemon.directory.path().join("pause.json");
            while !marker.exists() {
                assert!(
                    Instant::now() < daemon.deadline,
                    "{point}: {}",
                    daemon.stderr()
                );
                thread::sleep(Duration::from_millis(1));
            }
            queues.submit(2, VIRTIO_BLK_T_OUT, 0x33);
            daemon.child.kill().unwrap();
            daemon.child.wait().unwrap();
            fs::remove_file(daemon.directory.path().join("vhost.sock")).unwrap();
            fs::remove_file(daemon.directory.path().join("report.json")).unwrap();
            daemon.child = Daemon::start_kind(&daemon.directory, "local-async", false, true, None);
            daemon.deadline = Instant::now() + DEADLINE;
            queues.frontend = Frontend::from_stream(daemon.connect(), QUEUES as u64);
            queues.negotiate();
            for index in 0..3 {
                queues.configure(index, 0); // Deliberately stale frontend bases.
                queues.rings[index].kick.write(1).unwrap();
            }
            let before = queues.used(2);
            thread::sleep(Duration::from_millis(20));
            assert_eq!(
                queues.used(2),
                before,
                "admitted before queue 3 was restored"
            );
            assert!(
                daemon.child.try_wait().unwrap().is_none(),
                "{}",
                daemon.stderr()
            );
            queues.configure(3, 0);
            queues.rings[3].kick.write(1).unwrap();
            queues.wait(1, &daemon);
            queues.wait(2, &daemon);
            queues.submit(3, VIRTIO_BLK_T_FLUSH, 0);
            queues.wait(3, &daemon);
            queues.submit(0, VIRTIO_BLK_T_IN, 0);
            queues.wait(0, &daemon);
            let mut bytes = [0; BLOCK_SIZE];
            queues
                .mem
                .read_slice(&mut bytes, GuestAddress(0x5000))
                .unwrap();
            assert_eq!(bytes, [0x33; BLOCK_SIZE], "{point}, start {start}");
            drop(queues);
            let (status, report) = daemon.wait();
            assert!(status.success(), "{}", daemon.stderr());
            assert_eq!(report["queues"], 4);
            assert_eq!(report["errors"], 0);
            assert_eq!(report["local"]["status"]["published"], 3);
            assert_eq!(report["local"]["status"]["durable"], 3);
            assert_eq!(report["local"]["status"]["epoch"], 1);
        }
    }
}
