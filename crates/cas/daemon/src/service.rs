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
        let pending = backend.pending_count();
        let mut report = backend.report(pending, false);
        let fields = report.as_object_mut().expect("backend report object");
        fields.remove("connection_ok");
        fields.remove("pending_at_disconnect");
        fields.insert("pending".into(), pending.into());
        fields.insert("snapshot_timing".into(), serde_json::json!({
            "backend_lock_ns": crate::read_trace::ns(acquired.duration_since(requested)),
            "report_ns": crate::read_trace::ns(acquired.elapsed()),
            "completed_unix_ns": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(io::Error::other)?.as_nanos().min(u128::from(u64::MAX)) as u64
        }));
        Ok(report)
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
        let mut report = backend.report(
            pending_at_disconnect,
            result.is_ok() && drain_result.is_ok() && backend.failure().is_none(),
        );
        report["pending_after_drain"] = backend.pending_count().into();
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
