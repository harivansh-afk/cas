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
use vmm_sys_util::epoll::EventSet;

// The framework error does not implement std::error::Error.
fn daemon_error(error: DaemonError) -> io::Error {
    io::Error::other(error.to_string())
}

/// The caller preflights all socket/report paths before opening storage.
/// Listener creation refuses an existing socket; it never replaces one.
pub fn serve(backend: Backend, socket: &Path, report: File) -> io::Result<()> {
    let recovery_deadline = backend.recovery_deadline();
    let deadline_listener = backend.deadline_listener();
    let completion_fd = backend.completion_fd();
    let completion_token = backend.completion_token();
    let backend = Arc::new(Mutex::new(backend));
    let mut daemon = VhostUserDaemon::new(
        "cas-daemon".into(),
        backend.clone(),
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
    if let Some(deadline) = recovery_deadline {
        deadline.wait_readable(listener.as_raw_fd())?;
    }
    {
        // Workers cannot run before their fatal-error shutdown path is installed.
        // start accepts a connection and spawns the socket thread; it does not
        // call into Backend while this guard is held.
        let mut state = backend
            .lock()
            .map_err(|_| io::Error::other("backend worker panicked"))?;
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
    // Dropping the daemon joins workers. Do this before locking their backend.
    drop(daemon);
    eprintln!("cas-daemon: queue workers stopped");
    let mut backend = backend
        .lock()
        .map_err(|_| io::Error::other("backend worker panicked"))?;
    let pending_at_disconnect = backend.pending_count();
    eprintln!("cas-daemon: draining {pending_at_disconnect} requests");
    let drain_result = backend.drain();
    serde_json::to_writer_pretty(
        report,
        &backend.report(
            pending_at_disconnect,
            result.is_ok() && drain_result.is_ok() && backend.failure().is_none(),
        ),
    )?;
    if let Some(failure) = backend.failure() {
        return Err(io::Error::other(failure));
    }
    drain_result?;
    result.map_err(daemon_error)
}
