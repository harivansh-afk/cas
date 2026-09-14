//! Own a socket connection through framework shutdown and storage drain.
use crate::backend::Backend;
use std::{
    fs::File,
    io,
    os::fd::AsRawFd,
    path::Path,
    sync::{Arc, Mutex},
};
use vhost::vhost_user::{Error as ProtocolError, Listener};
use vhost_user_backend::{Error as DaemonError, VhostUserDaemon};
use vm_memory::{GuestMemoryAtomic, GuestMemoryMmap};
use vmm_sys_util::{
    epoll::EventSet,
    eventfd::{EFD_CLOEXEC, EFD_NONBLOCK, EventFd},
};

fn daemon_error(error: DaemonError) -> io::Error {
    io::Error::other(error.to_string())
}

/// The final report of one connection, written after its drain.
#[derive(serde::Serialize)]
struct FinalReport<'a> {
    #[serde(flatten)]
    backend: crate::backend::Report<'a>,
    connection_ok: bool,
    pending_at_disconnect: usize,
    pending_after_drain: usize,
}

/// A live sample of a connected backend, taken by the host telemetry loop.
#[derive(serde::Serialize)]
struct Snapshot<'a> {
    #[serde(flatten)]
    backend: crate::backend::Report<'a>,
    pending: usize,
    snapshot_timing: SnapshotTiming,
}

#[derive(serde::Serialize)]
struct SnapshotTiming {
    backend_lock_ns: u64,
    report_ns: u64,
    completed_unix_ns: u64,
}

pub struct Control {
    backend: Arc<Mutex<Backend>>,
    canceled: EventFd,
}
impl Control {
    pub(crate) fn snapshot(&self) -> io::Result<serde_json::Value> {
        let requested = std::time::Instant::now();
        let backend = self
            .backend
            .lock()
            .map_err(|_| io::Error::other("backend worker panicked"))?;
        let acquired = std::time::Instant::now();
        let report = backend.report();
        let report_ns = crate::read_trace::ns(acquired.elapsed());
        let completed_unix_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64;
        serde_json::to_value(Snapshot {
            backend: report,
            pending: backend.pending_count(),
            snapshot_timing: SnapshotTiming {
                backend_lock_ns: crate::read_trace::ns(acquired.duration_since(requested)),
                report_ns,
                completed_unix_ns,
            },
        })
        .map_err(io::Error::other)
    }

    /// Wake an unconnected listener and close any accepted frontend.
    pub fn cancel(&self, reason: &str) -> io::Result<()> {
        self.canceled.write(1)?;
        self.backend
            .lock()
            .map_err(|_| io::Error::other("backend worker panicked"))?
            .fail(reason.to_owned());
        Ok(())
    }
}

pub struct Service {
    backend: Arc<Mutex<Backend>>,
    canceled: EventFd,
    report: File,
}
impl Service {
    pub fn new(backend: Backend, report: File) -> io::Result<Self> {
        Ok(Self {
            backend: Arc::new(Mutex::new(backend)),
            canceled: EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK)?,
            report,
        })
    }
    pub fn control(&self) -> io::Result<Control> {
        Ok(Control {
            backend: Arc::clone(&self.backend),
            canceled: self.canceled.try_clone()?,
        })
    }
    /// Caller preflights all paths. Listener creation never replaces a socket.
    pub fn serve(self, socket: &Path) -> io::Result<()> {
        let result = self.connection(socket);
        // connection's daemon has dropped and joined its queue workers.
        let mut backend = self
            .backend
            .lock()
            .map_err(|_| io::Error::other("backend worker panicked"))?;
        if let Err(error) = &result {
            backend.fail(error.to_string());
        }
        let pending_at_disconnect = backend.pending_count();
        eprintln!("cas-daemon: draining {pending_at_disconnect} requests");
        let drain_result = backend.drain();
        let report = FinalReport {
            backend: backend.report(),
            connection_ok: result.is_ok() && drain_result.is_ok() && backend.failure().is_none(),
            pending_at_disconnect,
            pending_after_drain: backend.pending_count(),
        };
        serde_json::to_writer_pretty(self.report, &report)?;
        if let Some(failure) = backend.failure() {
            return Err(io::Error::other(failure));
        }
        drain_result?;
        result
    }
    fn connection(&self, socket: &Path) -> io::Result<()> {
        let (recovery_deadline, deadline_listener, completion_fd, completion_token) = {
            let backend = self
                .backend
                .lock()
                .map_err(|_| io::Error::other("backend worker panicked"))?;
            (
                backend.recovery_deadline(),
                backend.deadline_listener(),
                backend.completion_fd(),
                backend.completion_token(),
            )
        };
        let mut daemon = VhostUserDaemon::new(
            "cas-daemon".into(),
            Arc::clone(&self.backend),
            GuestMemoryAtomic::new(GuestMemoryMmap::new()),
        )
        .map_err(daemon_error)?;
        daemon.get_epoll_handlers()[0].register_listener(
            completion_fd,
            EventSet::IN,
            u64::from(completion_token),
        )?;
        if let Some((fd, token)) = deadline_listener {
            daemon.get_epoll_handlers()[0].register_listener(fd, EventSet::IN, u64::from(token))?;
        }
        let mut listener = Listener::new(socket, false)?;
        crate::deadline::wait_readable(
            listener.as_raw_fd(),
            Some(self.canceled.as_raw_fd()),
            recovery_deadline,
        )?;
        {
            let mut state = self
                .backend
                .lock()
                .map_err(|_| io::Error::other("backend worker panicked"))?;
            if let Some(error) = state.failure() {
                return Err(io::Error::other(error));
            }
            daemon.start(&mut listener).map_err(daemon_error)?;
            let shutdown = daemon
                .shutdown_handle()
                .ok_or_else(|| io::Error::other("missing connection shutdown handle"))?;
            state.set_shutdown_handle(shutdown);
        }
        eprintln!("cas-daemon: frontend connected");
        let result = match daemon.wait() {
            Err(DaemonError::HandleRequest(
                ProtocolError::Disconnected | ProtocolError::PartialMessage,
            )) => Ok(()),
            result => result,
        };
        eprintln!("cas-daemon: frontend disconnected: {result:?}");
        drop(daemon);
        eprintln!("cas-daemon: queue workers stopped");
        result.map_err(daemon_error)
    }
}

pub fn serve(backend: Backend, socket: &Path, report: File) -> io::Result<()> {
    Service::new(backend, report)?.serve(socket)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::net::UnixStream,
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };
    use vhost::{VhostBackend, vhost_user::Frontend};

    #[test]
    fn cancellation_wakes_a_listener_or_connection_and_reports_the_failure() {
        for connected in [false, true] {
            let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
            let image = directory.path().join("image.raw");
            File::create(&image).unwrap().set_len(4096).unwrap();
            let socket = directory.path().join("image.sock");
            let report = directory.path().join("report.json");
            let service = Service::new(
                Backend::new(&image).unwrap(),
                File::create(&report).unwrap(),
            )
            .unwrap();
            let control = service.control().unwrap();
            let (done, completed) = mpsc::channel();
            let service_socket = socket.clone();
            let worker = thread::spawn(move || done.send(service.serve(&service_socket)).unwrap());
            let deadline = Instant::now() + Duration::from_secs(2);
            while !socket.exists() {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(1));
            }
            let frontend = connected.then(|| {
                let stream = UnixStream::connect(&socket).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let frontend = Frontend::from_stream(stream, 1);
                frontend.set_owner().unwrap();
                assert_ne!(frontend.get_features().unwrap(), 0);
                frontend
            });
            control.cancel("supervisor stopped the host").unwrap();
            assert!(
                completed
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
                    .is_err()
            );
            worker.join().unwrap();
            let report: serde_json::Value =
                serde_json::from_reader(File::open(report).unwrap()).unwrap();
            assert_eq!(report["connection_ok"], false);
            assert_eq!(report["pending_after_drain"], 0);
            assert!(!socket.exists());
            drop((frontend, control));
        }
    }

    /// Key paths captured from the untyped `serde_json::json!` reports these
    /// structs replaced. Nested reports owned by other modules are not expanded.
    #[test]
    fn typed_reports_keep_the_untyped_key_set() {
        use crate::BackendKind;
        const COMMON: &[&str] = &[
            "admission",
            "backend",
            "bounce_requests",
            "errors",
            "fatal_error",
            "flush_negotiated",
            "flushes",
            "guest_payload_copy_bytes",
            "inflight",
            "local",
            "metadata",
            "negotiated_features",
            "peak_inflight",
            "queue_requests",
            "queues",
            "read_bytes",
            "read_progress",
            "read_trace",
            "reads",
            "restartable",
            "restored_pending",
            "restored_used",
            "schema_version",
            "staging",
            "write_bytes",
            "writes",
            "zero_bytes",
            "zeroes",
        ];
        const FINAL: &[&str] = &[
            "connection_ok",
            "pending_after_drain",
            "pending_at_disconnect",
        ];
        const SNAPSHOT: &[&str] = &[
            "pending",
            "snapshot_timing",
            "snapshot_timing.backend_lock_ns",
            "snapshot_timing.completed_unix_ns",
            "snapshot_timing.report_ns",
        ];
        const STAGING: &[&str] = &[
            "staging.appended",
            "staging.durable",
            "staging.image_bytes",
            "staging.log_bytes",
            "staging.mapped_blocks",
            "staging.recovered_tail_bytes",
        ];
        const READ_PROGRESS: &[&str] = &[
            "read_progress.bypassed_reads",
            "read_progress.descriptor_allocations",
            "read_progress.descriptor_reserve_bytes",
            "read_progress.discovered",
            "read_progress.read_admission",
            "read_progress.waiting_per_queue",
        ];
        const INFLIGHT: &[&str] = &[
            "inflight.active",
            "inflight.recovered_p",
            "inflight.replay_copy_bytes",
            "inflight.replayed_mutations",
            "inflight.replayed_requests",
            "inflight.replayed_write_bytes",
            "inflight.saved_p",
        ];
        fn paths(value: &serde_json::Value, prefix: &str, out: &mut Vec<String>) {
            let Some(fields) = value.as_object() else {
                return;
            };
            for (key, value) in fields {
                let path = format!("{prefix}{key}");
                if matches!(
                    path.as_str(),
                    "inflight" | "read_progress" | "staging" | "snapshot_timing"
                ) {
                    paths(value, &format!("{path}."), out);
                }
                out.push(path);
            }
        }
        fn expected(parts: &[&[&str]]) -> Vec<String> {
            let mut all: Vec<String> = parts
                .iter()
                .flat_map(|part| part.iter().map(|s| s.to_string()))
                .collect();
            all.sort();
            all
        }
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let image = directory.path().join("image.raw");
        File::create(&image).unwrap().set_len(8192).unwrap();
        let open = |name: &str, restartable| {
            Backend::open_with_recovery(
                &directory.path().join(name),
                BackendKind::LocalAsync,
                Some(8192),
                restartable,
                crate::fault::Fault::default(),
            )
            .unwrap()
        };
        let cases: [(Backend, &[&[&str]]); 4] = [
            (Backend::new(&image).unwrap(), &[]),
            (
                Backend::open(&directory.path().join("staging.log"), true, Some(8192)).unwrap(),
                &[STAGING],
            ),
            (open("local", false), &[READ_PROGRESS]),
            (open("live", true), &[READ_PROGRESS, INFLIGHT]),
        ];
        for (backend, extra) in cases {
            let report = serde_json::to_value(FinalReport {
                backend: backend.report(),
                connection_ok: true,
                pending_at_disconnect: 0,
                pending_after_drain: 0,
            })
            .unwrap();
            let mut found = Vec::new();
            paths(&report, "", &mut found);
            found.sort();
            assert_eq!(found, expected(&[&[COMMON, FINAL], extra].concat()));
            let service = Service::new(
                backend,
                File::create(directory.path().join("unused.json")).unwrap(),
            )
            .unwrap();
            let snapshot = service.control().unwrap().snapshot().unwrap();
            let mut found = Vec::new();
            paths(&snapshot, "", &mut found);
            found.sort();
            assert_eq!(found, expected(&[&[COMMON, SNAPSHOT], extra].concat()));
        }
    }

    #[test]
    fn rejected_listener_reports_failure_without_removing_the_existing_socket() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let image = directory.path().join("image.raw");
        File::create(&image).unwrap().set_len(4096).unwrap();
        let socket = directory.path().join("image.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let report = directory.path().join("report.json");
        assert!(
            serve(
                Backend::new(&image).unwrap(),
                &socket,
                File::create(&report).unwrap()
            )
            .is_err()
        );
        assert!(socket.exists());
        let report: serde_json::Value =
            serde_json::from_reader(File::open(report).unwrap()).unwrap();
        assert_eq!(report["connection_ok"], false);
        assert_eq!(report["pending_after_drain"], 0);
        drop(listener);
    }
}
