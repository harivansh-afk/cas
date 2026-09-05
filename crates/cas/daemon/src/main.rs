//! Single-queue vhost-user block device backed by a regular file and io_uring.
mod backend;
mod request;

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::Parser;
use vhost::vhost_user::{Error as ProtocolError, Listener};
use vhost_user_backend::{Error as DaemonError, VhostUserDaemon};
use vm_memory::{GuestMemoryAtomic, GuestMemoryMmap};
use vmm_sys_util::epoll::EventSet;

#[derive(Parser)]
#[command(about = "Serve an existing scratch raw image over vhost-user (one connection)")]
struct Args {
    #[arg(long)]
    socket: PathBuf,
    #[arg(long)]
    image: PathBuf,
    #[arg(long)]
    report: PathBuf,
}

// The upstream error does not implement std::error::Error.
fn daemon_error(error: DaemonError) -> io::Error {
    io::Error::other(error.to_string())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    // Do not replace someone else's socket or evidence.
    if args.socket.symlink_metadata().is_ok() {
        return Err("socket path already exists".into());
    }
    let report = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args.report)?;
    let backend = backend::Backend::new(&args.image)?;
    let completion_fd = backend.completion_fd();
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
        u64::from(backend::COMPLETION_EVENT),
    )?;
    let mut listener = Listener::new(&args.socket, false)?;
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
    backend.drain()?;
    serde_json::to_writer_pretty(
        report,
        &backend.report(
            pending_at_disconnect,
            result.is_ok() && backend.failure().is_none(),
        ),
    )?;
    if let Some(failure) = backend.failure() {
        return Err(io::Error::other(failure).into());
    }
    result.map_err(daemon_error)?;
    Ok(())
}
