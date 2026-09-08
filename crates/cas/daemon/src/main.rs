// Single-queue vhost-user block device with raw and staging storage modes.

mod backend;
mod fault;
mod request;
mod storage;

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::{Parser, ValueEnum};
use vhost::vhost_user::{Error as ProtocolError, Listener};
use vhost_user_backend::{Error as DaemonError, VhostUserDaemon};
use vm_memory::{GuestMemoryAtomic, GuestMemoryMmap};
use vmm_sys_util::epoll::EventSet;

#[derive(Clone, Copy, ValueEnum)]
enum BackendKind {
    Raw,
    Staging,
}

#[derive(Parser)]
#[command(about = "Serve a scratch raw image or staging log over vhost-user (one connection)")]
struct Args {
    #[arg(long)]
    socket: PathBuf,
    #[arg(long)]
    image: PathBuf,
    #[arg(long)]
    report: PathBuf,
    #[arg(long, value_enum, default_value = "raw")]
    backend: BackendKind,
    /// Create a new staging log with this logical capacity; never replaces a file.
    #[arg(long)]
    create_bytes: Option<u64>,
    /// Serial staging recovery: make each write durable before publishing completion.
    #[arg(long)]
    restartable: bool,
    /// Test-only pause at a write boundary; an external harness must kill/resume us.
    #[command(flatten)]
    fault: fault::FaultArgs,
}

// The upstream error does not implement std::error::Error.
fn daemon_error(error: DaemonError) -> io::Error {
    io::Error::other(error.to_string())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if args.create_bytes.is_some() && !matches!(args.backend, BackendKind::Staging) {
        return Err("--create-bytes requires --backend staging".into());
    }
    if args.restartable && !matches!(args.backend, BackendKind::Staging) {
        return Err("--restartable requires --backend staging".into());
    }
    let fault = args.fault.validate(args.restartable)?;
    // Do not replace someone else's socket or evidence.
    if args.socket.symlink_metadata().is_ok() {
        return Err("socket path already exists".into());
    }
    let report = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args.report)?;
    let backend = backend::Backend::open_with_recovery(
        &args.image,
        matches!(args.backend, BackendKind::Staging),
        args.create_bytes,
        args.restartable,
        fault,
    )?;
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
    let drain_result = backend.drain();
    serde_json::to_writer_pretty(
        report,
        &backend.report(
            pending_at_disconnect,
            result.is_ok() && drain_result.is_ok() && backend.failure().is_none(),
        ),
    )?;
    if let Some(failure) = backend.failure() {
        return Err(io::Error::other(failure).into());
    }
    drain_result?;
    result.map_err(daemon_error)?;
    Ok(())
}
