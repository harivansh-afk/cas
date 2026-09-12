//! Exercise the fork's FD hooks over an actual Unix socket, including a request
//! sent without negotiation (the typed frontend otherwise rejects it locally).

use std::fs::File;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use cas_daemon::inflight::{Carrier, Geometry, Identity, Kind, Request};
use vhost::vhost_user::message::{
    FrontendReq, VhostUserHeaderFlag, VhostUserInflight, VhostUserProtocolFeatures,
};
use vhost::vhost_user::{Frontend, Listener, VhostUserFrontend};
use vhost::{VhostBackend, VhostUserMemoryRegionInfo};
use vhost_user_backend::{StateChange, VhostUserBackendMut, VhostUserDaemon, VringRwLock};
use vm_memory::{
    ByteValued, Bytes, FileOffset, GuestAddress, GuestAddressSpace, GuestMemoryAtomic,
    GuestMemoryBackend, GuestMemoryLoadGuard, GuestMemoryMmap,
};
use vmm_sys_util::epoll::EventSet;
use vmm_sys_util::event::{
    EventConsumer, EventFlag, EventNotifier, new_event_consumer_and_notifier,
};

const IMAGE_BYTES: u64 = 1024 * 1024;
const IDENTITY: Identity = Identity {
    store: [3; 16],
    image: [7; 16],
    epoch: 9,
    attachment: 11,
};

struct Probe {
    carrier: Option<Carrier>,
    calls: usize,
    paused: bool,
    current: Option<GuestMemoryAtomic<GuestMemoryMmap>>,
    accepted: Option<GuestMemoryLoadGuard<GuestMemoryMmap>>,
    memory_changes: Vec<bool>,
    exit: (EventConsumer, EventNotifier),
}

impl VhostUserBackendMut for Probe {
    type Bitmap = ();
    type Vring = VringRwLock;

    fn num_queues(&self) -> usize {
        1
    }
    fn max_queue_size(&self) -> usize {
        256
    }
    fn features(&self) -> u64 {
        1 << 30
    } // VHOST_USER_F_PROTOCOL_FEATURES
    fn protocol_features(&self) -> VhostUserProtocolFeatures {
        VhostUserProtocolFeatures::INFLIGHT_SHMFD | VhostUserProtocolFeatures::REPLY_ACK
    }
    fn set_event_idx(&mut self, _: bool) {}
    fn update_memory(&mut self, memory: GuestMemoryAtomic<GuestMemoryMmap>) -> io::Result<()> {
        assert!(self.paused);
        self.accepted = Some(memory.memory());
        self.current = Some(memory);
        Ok(())
    }
    fn begin_state_change(&mut self, _: StateChange, _: &[VringRwLock]) -> io::Result<()> {
        assert!(!self.paused);
        if let (Some(current), Some(accepted)) = (&self.current, &self.accepted) {
            // The framework's live atomic map must still be the old map here.
            assert_eq!(
                current.memory().read_obj::<u64>(GuestAddress(0)).unwrap(),
                accepted.read_obj::<u64>(GuestAddress(0)).unwrap()
            );
        }
        self.paused = true;
        Ok(())
    }
    fn end_state_change(
        &mut self,
        change: StateChange,
        succeeded: bool,
        _: &[VringRwLock],
    ) -> io::Result<()> {
        assert!(self.paused);
        if change == StateChange::Memory {
            self.memory_changes.push(succeeded);
        }
        self.paused = false;
        Ok(())
    }
    fn exit_event(&self, _: usize) -> Option<(EventConsumer, EventNotifier)> {
        Some((
            self.exit.0.try_clone().unwrap(),
            self.exit.1.try_clone().unwrap(),
        ))
    }
    fn handle_event(&mut self, _: u16, _: EventSet, _: &[VringRwLock], _: usize) -> io::Result<()> {
        Err(io::Error::other("FD probe does not execute queues"))
    }
    fn get_inflight_fd(
        &mut self,
        message: &VhostUserInflight,
    ) -> io::Result<(VhostUserInflight, File)> {
        self.calls += 1;
        let geometry = Geometry::new(message.num_queues, message.queue_size)?;
        let mut carrier = Carrier::create(geometry, IDENTITY, IMAGE_BYTES, 0, metadata_budget())?;
        carrier.initialize_queue(0, 0, 0)?;
        carrier.admit(Request {
            kind: Kind::Write,
            queue: 0,
            head: 4,
            available: 0,
            offset: 0,
            length: 4096,
        })?;
        let exported = carrier.export()?;
        self.carrier = Some(carrier);
        Ok(exported)
    }
    fn set_inflight_fd(&mut self, message: &VhostUserInflight, file: File) -> io::Result<()> {
        self.calls += 1;
        self.carrier = Some(Carrier::attach(
            file,
            message,
            IDENTITY,
            IMAGE_BYTES,
            metadata_budget(),
        )?);
        Ok(())
    }
}

fn serve(
    run: impl FnOnce(&std::path::Path, &Arc<Mutex<Probe>>),
) -> (Arc<Mutex<Probe>>, Result<(), vhost_user_backend::Error>) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("inflight.sock");
    let mut listener = Listener::new(&path, false).unwrap();
    let probe = Arc::new(Mutex::new(Probe {
        carrier: None,
        calls: 0,
        paused: false,
        current: None,
        accepted: None,
        memory_changes: Vec::new(),
        exit: new_event_consumer_and_notifier(EventFlag::empty()).unwrap(),
    }));
    let server_probe = probe.clone();
    let server = thread::spawn(move || {
        let mut daemon = VhostUserDaemon::new(
            "inflight-probe".into(),
            server_probe,
            GuestMemoryAtomic::new(GuestMemoryMmap::new()),
        )
        .unwrap();
        daemon.start(&mut listener).unwrap();
        daemon.wait()
    });
    run(&path, &probe);
    // The frontend was dropped by run. A socket error is expected for the
    // deliberately unnegotiated message; either way the worker must exit.
    (probe, server.join().unwrap())
}

#[test]
fn negotiated_fd_round_trip_retains_metadata_after_backend_drops_its_copy() {
    let (_, result) = serve(|path, probe| {
        let stream = UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut frontend = Frontend::from_stream(stream, 1);
        let features = frontend.get_features().unwrap();
        frontend.set_features(features).unwrap();
        let protocol = frontend.get_protocol_features().unwrap();
        frontend.set_protocol_features(protocol).unwrap();
        frontend.set_hdr_flags(VhostUserHeaderFlag::NEED_REPLY);
        let requested = VhostUserInflight::new(0, 0, 1, 256);
        let (message, file) = frontend.get_inflight_fd(&requested).unwrap();
        probe.lock().unwrap().carrier.take();
        frontend
            .set_inflight_fd(&message, file.as_raw_fd())
            .unwrap();
        drop(file);
        let mut state = probe.lock().unwrap();
        let replay = state
            .carrier
            .as_mut()
            .unwrap()
            .reconcile(&[Some(0)])
            .unwrap();
        assert_eq!(state.calls, 2);
        assert_eq!(replay.entries.len(), 1);
        assert_eq!(
            (replay.entries[0].serial, replay.entries[0].mutation),
            (1, 1)
        );
    });
    assert!(matches!(
        result,
        Err(vhost_user_backend::Error::HandleRequest(
            vhost::vhost_user::Error::Disconnected
        ))
    ));
}

#[test]
fn unnegotiated_get_never_reaches_backend_hook() {
    let (probe, result) = serve(|path, _| {
        let mut stream = UnixStream::connect(path).unwrap();
        let message = VhostUserInflight::new(0, 0, 1, 256);
        // Bypass Frontend's local negotiation check. The standard header is
        // three little-endian u32 fields: request, flags/version, body length.
        for word in [
            FrontendReq::GET_INFLIGHT_FD as u32,
            1,
            size_of::<VhostUserInflight>() as u32,
        ] {
            stream.write_all(&word.to_le_bytes()).unwrap();
        }
        stream.write_all(message.as_slice()).unwrap();
        // Dropping the stream ends the server after it handles this request.
        drop(stream);
    });
    assert_eq!(probe.lock().unwrap().calls, 0);
    assert!(matches!(result,
        Err(vhost_user_backend::Error::HandleRequest(vhost::vhost_user::Error::InactiveOperation(feature)))
            if feature == VhostUserProtocolFeatures::INFLIGHT_SHMFD));
}

#[test]
fn state_change_hooks_bracket_atomic_memory_replacement_and_failed_updates() {
    let (probe, _) = serve(|path, probe| {
        let stream = UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut frontend = Frontend::from_stream(stream, 1);
        let features = frontend.get_features().unwrap();
        frontend.set_features(features).unwrap();
        let protocol = frontend.get_protocol_features().unwrap();
        frontend.set_protocol_features(protocol).unwrap();
        frontend.set_hdr_flags(VhostUserHeaderFlag::NEED_REPLY);
        for value in [0x11u64, 0x22] {
            let file = tempfile::tempfile().unwrap();
            file.set_len(4096).unwrap();
            let mem = GuestMemoryMmap::<()>::from_ranges_with_files([(
                GuestAddress(0),
                4096,
                Some(FileOffset::new(file, 0)),
            )])
            .unwrap();
            mem.write_obj(value, GuestAddress(0)).unwrap();
            let region =
                VhostUserMemoryRegionInfo::from_guest_region(mem.iter().next().unwrap()).unwrap();
            frontend.set_mem_table(&[region]).unwrap();
            assert_eq!(
                probe
                    .lock()
                    .unwrap()
                    .accepted
                    .as_ref()
                    .unwrap()
                    .read_obj::<u64>(GuestAddress(0))
                    .unwrap(),
                value
            );
            if value == 0x22 {
                // Overlapping regions fail before replacing the accepted map.
                assert!(frontend.set_mem_table(&[region, region]).is_err());
            }
        }
    });
    let probe = probe.lock().unwrap();
    assert_eq!(probe.memory_changes, [true, true, false]);
    assert_eq!(
        probe
            .accepted
            .as_ref()
            .unwrap()
            .read_obj::<u64>(GuestAddress(0))
            .unwrap(),
        0x22
    );
    assert!(!probe.paused);
}

fn metadata_budget() -> std::sync::Arc<cas_core::budget::Budget> {
    cas_core::budget::Budget::new(cas_core::budget::Amount {
        bytes: 128 * 1024 * 1024,
        requests: 0,
    })
}
