// Single-queue vhost-user block device with raw and staging storage modes.

mod backend;
mod fault;
mod local;
mod request;
mod storage;

use std::io;
use std::num::NonZeroU64;
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
    Local,
    LocalAsync,
}

#[derive(Parser)]
#[command(
    about = "Serve a raw image, reference staging log or local append directory over vhost-user"
)]
struct Args {
    #[arg(long)]
    socket: PathBuf,
    #[arg(long)]
    image: PathBuf,
    #[arg(long)]
    report: PathBuf,
    #[arg(long, value_enum, default_value = "raw")]
    backend: BackendKind,
    /// Create new staging/local storage with this logical capacity; never replaces existing data.
    #[arg(long)]
    create_bytes: Option<u64>,
    /// Live recovery: serial durable staging, or retained inflight state for local-async.
    #[arg(long)]
    restartable: bool,
    /// Test-only pause at a write boundary; an external harness must kill/resume us.
    #[command(flatten)]
    pause: Option<PauseArgs>,
}

#[derive(clap::Args)]
#[group(requires_all = ["pause_at", "pause_after", "pause_marker"], requires = "restartable")]
struct PauseArgs {
    // Requirements belong to the optional group, not to ordinary daemon runs.
    #[arg(long, value_enum, required = false)]
    pause_at: fault::Point,
    #[arg(long, required = false)]
    pause_after: NonZeroU64,
    #[arg(long, required = false)]
    pause_marker: PathBuf,
}

impl TryFrom<PauseArgs> for fault::Pause {
    type Error = io::Error;

    fn try_from(args: PauseArgs) -> io::Result<Self> {
        Self::new(args.pause_at, args.pause_after, args.pause_marker)
    }
}

// The upstream error does not implement std::error::Error.
fn daemon_error(error: DaemonError) -> io::Error {
    io::Error::other(error.to_string())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if args.create_bytes.is_some() && matches!(args.backend, BackendKind::Raw) {
        return Err("--create-bytes requires --backend staging or local".into());
    }
    if args.restartable && !matches!(args.backend, BackendKind::Staging | BackendKind::LocalAsync) {
        return Err("--restartable requires --backend staging or local-async".into());
    }
    let fault = fault::Fault::new(args.pause.map(TryInto::try_into).transpose()?);
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
        args.backend,
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

#[cfg(test)]
mod cli_tests {
    use super::*;

    #[test]
    fn pause_options_are_optional_but_require_a_complete_restartable_configuration() {
        let base = [
            "cas-daemon",
            "--socket",
            "socket",
            "--image",
            "image",
            "--report",
            "report",
        ];
        assert!(Args::try_parse_from(base).unwrap().pause.is_none());
        let valid = [
            "--restartable",
            "--pause-at",
            "after-storage",
            "--pause-after",
            "32",
            "--pause-marker",
            "pause.json",
        ];
        assert!(
            Args::try_parse_from(base.into_iter().chain(valid))
                .unwrap()
                .pause
                .is_some()
        );
        for invalid in [
            vec!["--pause-at", "after-storage"],
            vec!["--restartable", "--pause-after", "32"],
            vec![
                "--pause-at",
                "after-storage",
                "--pause-after",
                "32",
                "--pause-marker",
                "pause.json",
            ],
            vec![
                "--restartable",
                "--pause-at",
                "after-storage",
                "--pause-after",
                "0",
                "--pause-marker",
                "pause.json",
            ],
        ] {
            assert!(Args::try_parse_from(base.into_iter().chain(invalid)).is_err());
        }
    }
}
