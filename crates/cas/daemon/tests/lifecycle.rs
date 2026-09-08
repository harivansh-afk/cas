// Real socket/worker regressions
// Requires host io_uring and direct file IO.

use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::FileExt;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use cas_core::{
    BLOCK_SIZE,
    staging::{RECORD_SIZE, StagingLog},
};
use tempfile::TempDir;
use vhost::vhost_user::{Frontend, VhostUserFrontend};
use vhost::{VhostBackend, VhostUserMemoryRegionInfo, VringConfigData};
use virtio_bindings::bindings::virtio_blk::{
    VIRTIO_BLK_S_IOERR, VIRTIO_BLK_S_OK, VIRTIO_BLK_T_FLUSH, VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT,
};
use virtio_bindings::bindings::virtio_ring::{VRING_DESC_F_NEXT, VRING_DESC_F_WRITE};
use vm_memory::{Bytes, FileOffset, GuestAddress, GuestMemoryBackend, GuestMemoryMmap};
use vmm_sys_util::eventfd::{EFD_CLOEXEC, EFD_NONBLOCK, EventFd};

// Linux 6.17.13 can leave io-wq workers asleep until their five-second idle timeout.
const DEADLINE: Duration = Duration::from_secs(10);

struct Daemon {
    child: Child,
    directory: TempDir,
    deadline: Instant,
}

impl Daemon {
    fn spawn() -> Self {
        Self::spawn_backend(false)
    }

    fn spawn_backend(staging: bool) -> Self {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        if !staging {
            File::create(directory.path().join("image.raw"))
                .unwrap()
                .set_len(BLOCK_SIZE as u64)
                .unwrap();
        }
        let child = Self::start(&directory, staging, staging);
        Self {
            child,
            directory,
            deadline: Instant::now() + DEADLINE,
        }
    }

    fn start(directory: &TempDir, staging: bool, create: bool) -> Child {
        Self::start_options(directory, staging, create, false, None)
    }

    fn start_options(
        directory: &TempDir,
        staging: bool,
        create: bool,
        restartable: bool,
        pause: Option<&str>,
    ) -> Child {
        let stderr = File::create(directory.path().join("stderr")).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_cas-daemon"));
        command
            .arg("--socket")
            .arg(directory.path().join("vhost.sock"))
            .arg("--image")
            .arg(directory.path().join("image.raw"))
            .arg("--report")
            .arg(directory.path().join("report.json"));
        if staging {
            command.args(["--backend", "staging"]);
        }
        if create {
            command.arg("--create-bytes").arg(BLOCK_SIZE.to_string());
        }
        if restartable {
            command.arg("--restartable");
        }
        if let Some(point) = pause {
            command
                .args(["--pause-at", point, "--pause-after", "2"])
                .arg("--pause-marker")
                .arg(directory.path().join("pause.json"));
        }
        command
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .unwrap()
    }

    fn crash_and_reopen(&mut self) {
        self.child.kill().unwrap();
        assert!(!self.child.wait().unwrap().success());
        fs::remove_file(self.directory.path().join("vhost.sock")).unwrap();
        fs::remove_file(self.directory.path().join("report.json")).unwrap();
        self.child = Self::start(&self.directory, true, false);
        self.deadline = Instant::now() + DEADLINE;
    }

    fn stderr(&self) -> String {
        fs::read_to_string(self.directory.path().join("stderr")).unwrap()
    }

    fn connect(&mut self) -> UnixStream {
        loop {
            match UnixStream::connect(self.directory.path().join("vhost.sock")) {
                Ok(stream) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    return stream;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) => {}
                Err(error) => panic!("connect failed: {error}"),
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!(
                    "daemon exited before connecting: {status}; {}",
                    self.stderr()
                );
            }
            assert!(
                Instant::now() < self.deadline,
                "socket deadline exceeded; {}",
                self.stderr()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait(&mut self) -> (ExitStatus, serde_json::Value) {
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < self.deadline,
                "daemon exit deadline exceeded; {}",
                self.stderr()
            );
            thread::sleep(Duration::from_millis(10));
        };
        let report = fs::read(self.directory.path().join("report.json")).unwrap();
        let report = serde_json::from_slice(&report)
            .unwrap_or_else(|error| panic!("invalid report: {error}; {}", self.stderr()));
        (status, report)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Also runs on assertion failure so as to never leave a blocked daemon behind
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct FrontendQueue {
    frontend: Frontend,
    observer: UnixStream,
    mem: GuestMemoryMmap,
    call: EventFd,
    kick: EventFd,
    next_avail: u16,
    deadline: Instant,
}

impl FrontendQueue {
    fn connect(daemon: &mut Daemon) -> Self {
        let stream = daemon.connect();
        let observer = stream.try_clone().unwrap();
        let mut frontend = Frontend::from_stream(stream, 1);
        let file = tempfile::tempfile_in(daemon.directory.path()).unwrap();
        file.set_len(0x10000).unwrap();
        let mem = GuestMemoryMmap::from_ranges_with_files([(
            GuestAddress(0),
            0x10000,
            Some(FileOffset::new(file, 0)),
        )])
        .unwrap();
        let call = EventFd::new(EFD_NONBLOCK | EFD_CLOEXEC).unwrap();
        let kick = EventFd::new(EFD_NONBLOCK | EFD_CLOEXEC).unwrap();
        Self::configure(&mut frontend, &mem, &call, &kick);
        Self {
            frontend,
            observer,
            mem,
            call,
            kick,
            next_avail: 0,
            deadline: daemon.deadline,
        }
    }

    fn configure(frontend: &mut Frontend, mem: &GuestMemoryMmap, call: &EventFd, kick: &EventFd) {
        frontend.set_owner().unwrap();
        let features = frontend.get_features().unwrap();
        frontend.set_features(features).unwrap();
        let protocol_features = frontend.get_protocol_features().unwrap();
        frontend.set_protocol_features(protocol_features).unwrap();

        let region =
            VhostUserMemoryRegionInfo::from_guest_region(mem.iter().next().unwrap()).unwrap();
        frontend.set_mem_table(&[region]).unwrap();
        frontend.set_vring_num(0, 128).unwrap();
        frontend
            .set_vring_addr(
                0,
                &VringConfigData {
                    queue_max_size: 128,
                    queue_size: 128,
                    desc_table_addr: region.userspace_addr + 0x1000,
                    avail_ring_addr: region.userspace_addr + 0x2000,
                    used_ring_addr: region.userspace_addr + 0x3000,
                    ..Default::default()
                },
            )
            .unwrap();
        frontend.set_vring_base(0, 0).unwrap();
        frontend.set_vring_call(0, call).unwrap();
        frontend.set_vring_kick(0, kick).unwrap();
        frontend.set_vring_enable(0, true).unwrap();
        // A reply orders all preceding setup messages before the first kick.
        frontend.get_features().unwrap();
    }

    fn descriptor(&self, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let mut descriptor = [0; 16];
        descriptor[..8].copy_from_slice(&addr.to_le_bytes());
        descriptor[8..12].copy_from_slice(&len.to_le_bytes());
        descriptor[12..14].copy_from_slice(&flags.to_le_bytes());
        descriptor[14..].copy_from_slice(&next.to_le_bytes());
        self.mem
            .write_slice(&descriptor, GuestAddress(0x1000 + u64::from(index) * 16))
            .unwrap();
    }

    fn kick(&mut self) {
        self.kick_head(0);
    }

    fn kick_head(&mut self, head: u16) {
        let slot = 0x2004 + u64::from(self.next_avail % 128) * 2;
        self.mem
            .write_obj(head.to_le(), GuestAddress(slot))
            .unwrap();
        self.next_avail = self.next_avail.wrapping_add(1);
        self.mem
            .write_obj(self.next_avail.to_le(), GuestAddress(0x2002))
            .unwrap();
        self.kick.write(1).unwrap();
    }

    fn request_layout(&mut self, kind: u32, fragmented: bool) {
        self.request_status(kind, fragmented, VIRTIO_BLK_S_OK as u8);
    }

    fn request_status(&mut self, kind: u32, fragmented: bool, expected_status: u8) {
        let mut header = [0; 16];
        header[..4].copy_from_slice(&kind.to_le_bytes());
        self.mem.write_slice(&header, GuestAddress(0x4000)).unwrap();
        self.mem.write_obj(0xffu8, GuestAddress(0x6000)).unwrap();
        self.descriptor(0, 0x4000, 16, VRING_DESC_F_NEXT as u16, 1);
        let status_index = if kind == VIRTIO_BLK_T_FLUSH {
            1
        } else {
            let flags = VRING_DESC_F_NEXT
                | if kind == VIRTIO_BLK_T_IN {
                    VRING_DESC_F_WRITE
                } else {
                    0
                };
            self.descriptor(1, 0x5000, BLOCK_SIZE as u32, flags as u16, 2);
            2
        };
        self.descriptor(status_index, 0x6000, 1, VRING_DESC_F_WRITE as u16, 0);
        if fragmented {
            if kind == VIRTIO_BLK_T_OUT {
                // Split the header, then share its final bytes with the data.
                self.mem.write_slice(&header, GuestAddress(0x4ff0)).unwrap();
                self.descriptor(0, 0x4ff0, 8, VRING_DESC_F_NEXT as u16, 1);
                self.descriptor(
                    1,
                    0x4ff8,
                    8 + BLOCK_SIZE as u32,
                    VRING_DESC_F_NEXT as u16,
                    2,
                );
            } else {
                self.descriptor(0, 0x4000, 8, VRING_DESC_F_NEXT as u16, 1);
                self.descriptor(1, 0x4008, 8, VRING_DESC_F_NEXT as u16, 2);
                let (addr, len) = if kind == VIRTIO_BLK_T_IN {
                    (0x5000, BLOCK_SIZE as u32 + 1)
                } else {
                    (0x6000, 1)
                };
                // Read payload and status share the final writable descriptor.
                self.descriptor(2, addr, len, VRING_DESC_F_WRITE as u16, 0);
            }
        }
        self.kick();
        loop {
            match self.call.read() {
                Ok(_) => break,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < self.deadline,
                        "request completion deadline exceeded"
                    );
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("completion notification failed: {error}"),
            }
        }
        assert_eq!(
            self.mem.read_obj::<u8>(GuestAddress(0x6000)).unwrap(),
            expected_status
        );
        assert_eq!(
            u16::from_le(self.mem.read_obj::<u16>(GuestAddress(0x3002)).unwrap()),
            self.next_avail
        );
    }
}

#[test]
fn frontend_disconnect_exits_cleanly() {
    let mut daemon = Daemon::spawn();
    drop(daemon.connect());
    let (status, report) = daemon.wait();
    assert!(status.success(), "{}", daemon.stderr());
    assert_eq!(report["connection_ok"], true);
    assert!(report["fatal_error"].is_null());
    assert_eq!(report["pending_at_disconnect"], 0);
}

#[test]
fn serial_recovery_replays_in_order_across_write_boundaries_and_ring_wrap() {
    for point in [
        "before-submit",
        "after-storage",
        "after-status",
        "after-used",
    ] {
        for start in [0u16, u16::MAX - 1] {
            let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
            let child = Daemon::start_options(&directory, true, true, true, Some(point));
            let mut daemon = Daemon {
                child,
                directory,
                deadline: Instant::now() + DEADLINE,
            };
            let mut queue = FrontendQueue::connect(&mut daemon);
            // The backend must derive both cursors from guest memory, including
            // when SET_VRING_BASE contains a stale zero from the frontend.
            queue
                .mem
                .write_obj(start.to_le(), GuestAddress(0x3002))
                .unwrap();
            queue
                .mem
                .write_obj(start.to_le(), GuestAddress(0x2002))
                .unwrap();
            queue.next_avail = start;
            queue
                .mem
                .write_slice(&[0x11; BLOCK_SIZE], GuestAddress(0x5000))
                .unwrap();
            queue.request_layout(VIRTIO_BLK_T_OUT, false); // acknowledged, no FLUSH
            for (head, base, value) in [(3, 0x7000, 0x22), (6, 0xa000, 0x33)] {
                queue
                    .mem
                    .write_obj(VIRTIO_BLK_T_OUT.to_le(), GuestAddress(base))
                    .unwrap();
                queue
                    .mem
                    .write_slice(&[value; BLOCK_SIZE], GuestAddress(base + 0x100))
                    .unwrap();
                queue
                    .mem
                    .write_obj(0xffu8, GuestAddress(base + 0x1100))
                    .unwrap();
                queue.descriptor(head, base, 16, VRING_DESC_F_NEXT as u16, head + 1);
                queue.descriptor(
                    head + 1,
                    base + 0x100,
                    BLOCK_SIZE as u32,
                    VRING_DESC_F_NEXT as u16,
                    head + 2,
                );
                queue.descriptor(head + 2, base + 0x1100, 1, VRING_DESC_F_WRITE as u16, 0);
                queue.kick_head(head);
            }
            let marker = daemon.directory.path().join("pause.json");
            while !marker.exists() {
                assert!(
                    Instant::now() < daemon.deadline,
                    "pause timeout: {}",
                    daemon.stderr()
                );
                assert!(
                    daemon.child.try_wait().unwrap().is_none(),
                    "{}",
                    daemon.stderr()
                );
                thread::sleep(Duration::from_millis(1));
            }
            let marker: serde_json::Value =
                serde_json::from_slice(&fs::read(marker).unwrap()).unwrap();
            assert_eq!(marker["point"], point);
            assert_eq!(marker["writes"], 2);
            daemon.child.kill().unwrap();
            daemon.child.wait().unwrap();
            // The first write was acknowledged without a guest FLUSH and must
            // survive. A stored-but-unpublished second write may survive too.
            {
                let log = StagingLog::open(daemon.directory.path().join("image.raw")).unwrap();
                assert_eq!(
                    log.read(0, BLOCK_SIZE).unwrap(),
                    vec![if point == "before-submit" { 0x11 } else { 0x22 }; BLOCK_SIZE]
                );
            }
            fs::remove_file(daemon.directory.path().join("vhost.sock")).unwrap();
            fs::remove_file(daemon.directory.path().join("report.json")).unwrap();
            daemon.child = Daemon::start_options(&daemon.directory, true, false, true, None);
            daemon.deadline = Instant::now() + DEADLINE;
            let stream = daemon.connect();
            queue.observer = stream.try_clone().unwrap();
            queue.frontend = Frontend::from_stream(stream, 1);
            while queue.call.read().is_ok() {}
            FrontendQueue::configure(&mut queue.frontend, &queue.mem, &queue.call, &queue.kick);
            queue.kick.write(1).unwrap();
            while u16::from_le(queue.mem.read_obj::<u16>(GuestAddress(0x3002)).unwrap())
                != queue.next_avail
            {
                assert!(
                    Instant::now() < daemon.deadline,
                    "replay timeout: {}",
                    daemon.stderr()
                );
                thread::sleep(Duration::from_millis(1));
            }
            while queue.call.read().is_ok() {}
            queue.deadline = daemon.deadline;
            queue.request_layout(VIRTIO_BLK_T_IN, false);
            let mut bytes = [0; BLOCK_SIZE];
            queue
                .mem
                .read_slice(&mut bytes, GuestAddress(0x5000))
                .unwrap();
            assert_eq!(bytes, [0x33; BLOCK_SIZE], "{point}, start={start}");
            drop(queue);
            let (status, report) = daemon.wait();
            assert!(status.success(), "{}", daemon.stderr());
            assert_eq!(report["errors"], 0);
            assert_eq!(report["peak_inflight"], 1);
            assert_eq!(report["writes"], if point == "after-used" { 1 } else { 2 });
            assert_eq!(
                report["restored_used"],
                start.wrapping_add(if point == "after-used" { 2 } else { 1 })
            );
            assert_eq!(
                report["staging"]["durable"],
                if matches!(point, "after-storage" | "after-status") {
                    4
                } else {
                    3
                }
            );
        }
    }
}

#[test]
fn completed_write_flush_read_exits_cleanly() {
    write_flush_read(false, false);
}

#[test]
fn fragmented_write_flush_read_exits_cleanly() {
    write_flush_read(false, true);
}

#[test]
fn staging_fragmented_write_flush_read_exits_cleanly() {
    write_flush_read(true, true);
}

fn write_flush_read(staging: bool, fragmented: bool) {
    let mut daemon = Daemon::spawn_backend(staging);
    let mut queue = FrontendQueue::connect(&mut daemon);
    let expected = [0x5a; BLOCK_SIZE];
    queue
        .mem
        .write_slice(&expected, GuestAddress(0x5000))
        .unwrap();
    queue.request_layout(VIRTIO_BLK_T_OUT, fragmented);
    queue.request_layout(VIRTIO_BLK_T_FLUSH, fragmented);
    queue
        .mem
        .write_slice(&[0; BLOCK_SIZE], GuestAddress(0x5000))
        .unwrap();
    queue.request_layout(VIRTIO_BLK_T_IN, fragmented);
    let mut actual = [0; BLOCK_SIZE];
    queue
        .mem
        .read_slice(&mut actual, GuestAddress(0x5000))
        .unwrap();
    assert_eq!(actual, expected);
    // A control round trip after IO also proves the frontend remains responsive.
    queue.frontend.get_features().unwrap();
    drop(queue);
    let (status, report) = daemon.wait();
    assert!(status.success(), "{}", daemon.stderr());
    assert_eq!(report["connection_ok"], true);
    assert!(report["fatal_error"].is_null());
    assert_eq!(report["flush_negotiated"], true);
    assert_eq!(report["pending_at_disconnect"], 0);
    assert_eq!(report["errors"], 0);
    assert_eq!(report["writes"], 1);
    assert_eq!(report["reads"], 1);
    assert_eq!(report["flushes"], 1);
    assert_eq!(report["write_bytes"], BLOCK_SIZE);
    assert_eq!(report["read_bytes"], BLOCK_SIZE);
    let path = daemon.directory.path().join("image.raw");
    let actual = if staging {
        assert_eq!(report["backend"], "staging_sync");
        assert_eq!(report["staging"]["appended"], 1);
        assert_eq!(report["staging"]["durable"], 1);
        StagingLog::open(path).unwrap().read(0, BLOCK_SIZE).unwrap()
    } else {
        fs::read(path).unwrap()
    };
    assert_eq!(actual, expected);
}

#[test]
fn missing_required_features_closes_frontend_before_io() {
    use virtio_bindings::bindings::{
        virtio_blk::{VIRTIO_BLK_F_BLK_SIZE, VIRTIO_BLK_F_FLUSH},
        virtio_config::VIRTIO_F_VERSION_1,
    };
    for feature in [
        VIRTIO_BLK_F_FLUSH,
        VIRTIO_BLK_F_BLK_SIZE,
        VIRTIO_F_VERSION_1,
    ] {
        let mut daemon = Daemon::spawn();
        let stream = daemon.connect();
        let mut observer = stream.try_clone().unwrap();
        let frontend = Frontend::from_stream(stream, 1);
        frontend.set_owner().unwrap();
        let features = frontend.get_features().unwrap() & !(1 << feature);
        frontend.set_features(features).unwrap();
        let (status, report) = daemon.wait();
        assert!(!status.success(), "{}", daemon.stderr());
        assert_eq!(
            report["fatal_error"],
            "guest must negotiate VERSION_1, BLK_SIZE, and FLUSH"
        );
        assert_eq!(report["negotiated_features"], features);
        assert_eq!(report["writes"], 0);
        assert_eq!(report["pending_at_disconnect"], 0);
        assert_eq!(
            fs::read(daemon.directory.path().join("image.raw")).unwrap(),
            vec![0; BLOCK_SIZE]
        );
        assert_eq!(observer.read(&mut [0]).unwrap(), 0);
    }
}

#[test]
fn malformed_chain_closes_frontend_and_reports_worker_failure() {
    let mut daemon = Daemon::spawn();
    let mut queue = FrontendQueue::connect(&mut daemon);
    // One readable descriptor, with neither NEXT nor a writable status byte.
    queue.descriptor(0, 0x4000, 16, 0, 0);
    queue.kick();

    // Keep the frontend alive: worker failure must cause shutdown itself.
    let (status, report) = daemon.wait();
    assert!(!status.success());
    assert_eq!(report["connection_ok"], false);
    assert_eq!(
        report["fatal_error"],
        "malformed request without writable status"
    );
    assert_eq!(report["pending_at_disconnect"], 0);
    assert_eq!(
        queue.observer.read(&mut [0]).unwrap(),
        0,
        "frontend socket remained open"
    );
}

#[test]
fn queued_staging_flush_survives_kill_and_discards_later_overwrite() {
    let mut daemon = Daemon::spawn_backend(true);
    let mut queue = FrontendQueue::connect(&mut daemon);
    // Submit all four requests without waiting: A, FLUSH, B, READ. Each request
    // has separate guest buffers, descriptors, and status storage.
    for (index, (kind, value)) in [
        (VIRTIO_BLK_T_OUT, 0x5a),
        (VIRTIO_BLK_T_FLUSH, 0),
        (VIRTIO_BLK_T_OUT, 0xa5),
        (VIRTIO_BLK_T_IN, 0),
    ]
    .into_iter()
    .enumerate()
    {
        let base = 0x4000 + index as u64 * 0x2000;
        let head = index as u16 * 3;
        queue
            .mem
            .write_obj(kind.to_le(), GuestAddress(base))
            .unwrap();
        queue
            .mem
            .write_slice(&[value; BLOCK_SIZE], GuestAddress(base + 0x100))
            .unwrap();
        queue
            .mem
            .write_obj(0xffu8, GuestAddress(base + 0x1100))
            .unwrap();
        queue.descriptor(head, base, 16, VRING_DESC_F_NEXT as u16, head + 1);
        let status_index = if kind == VIRTIO_BLK_T_FLUSH {
            head + 1
        } else {
            let flags = VRING_DESC_F_NEXT
                | if kind == VIRTIO_BLK_T_IN {
                    VRING_DESC_F_WRITE
                } else {
                    0
                };
            queue.descriptor(
                head + 1,
                base + 0x100,
                BLOCK_SIZE as u32,
                flags as u16,
                head + 2,
            );
            head + 2
        };
        queue.descriptor(status_index, base + 0x1100, 1, VRING_DESC_F_WRITE as u16, 0);
        queue.kick_head(head);
    }
    while u16::from_le(queue.mem.read_obj::<u16>(GuestAddress(0x3002)).unwrap()) != 4 {
        assert!(
            Instant::now() < queue.deadline,
            "queued requests did not complete; {}",
            daemon.stderr()
        );
        thread::sleep(Duration::from_millis(1));
    }
    for index in 0..4 {
        assert_eq!(
            queue
                .mem
                .read_obj::<u8>(GuestAddress(0x5100 + index * 0x2000))
                .unwrap(),
            VIRTIO_BLK_S_OK as u8
        );
    }
    let mut bytes = [0; BLOCK_SIZE];
    queue
        .mem
        .read_slice(&mut bytes, GuestAddress(0xa100))
        .unwrap();
    assert_eq!(bytes, [0xa5; BLOCK_SIZE]);
    daemon.crash_and_reopen();
    drop(queue);
    let mut queue = FrontendQueue::connect(&mut daemon);
    queue.request_layout(VIRTIO_BLK_T_IN, true);
    queue
        .mem
        .read_slice(&mut bytes, GuestAddress(0x5000))
        .unwrap();
    assert_eq!(bytes, [0x5a; BLOCK_SIZE]);
    drop(queue);
    let (status, report) = daemon.wait();
    assert!(status.success(), "{}", daemon.stderr());
    assert_eq!(report["errors"], 0);
    assert_eq!(report["staging"]["durable"], 1);
    assert_eq!(report["staging"]["appended"], 1);
    assert_eq!(report["staging"]["recovered_tail_bytes"], RECORD_SIZE);
}

#[test]
fn staging_corruption_reports_ioerr_and_stops_the_connection() {
    let mut daemon = Daemon::spawn_backend(true);
    let mut queue = FrontendQueue::connect(&mut daemon);
    queue
        .mem
        .write_slice(&[0x5a; BLOCK_SIZE], GuestAddress(0x5000))
        .unwrap();
    queue.request_layout(VIRTIO_BLK_T_OUT, false);
    queue.request_layout(VIRTIO_BLK_T_FLUSH, false);
    // Corrupt a committed payload through an independent descriptor. The next
    // guest read must report the checksum failure, never return unchecked bytes.
    File::options()
        .write(true)
        .open(daemon.directory.path().join("image.raw"))
        .unwrap()
        .write_all_at(&[0], (2 * BLOCK_SIZE) as u64)
        .unwrap();
    queue.request_status(VIRTIO_BLK_T_IN, false, VIRTIO_BLK_S_IOERR as u8);
    let (status, report) = daemon.wait();
    assert!(!status.success());
    assert_eq!(report["connection_ok"], false);
    assert!(
        report["fatal_error"]
            .as_str()
            .unwrap()
            .contains("corrupt record")
    );
    assert_eq!(report["reads"], 0);
    assert_eq!(report["staging"]["durable"], 1);
    assert_eq!(queue.observer.read(&mut [0]).unwrap(), 0);
}
