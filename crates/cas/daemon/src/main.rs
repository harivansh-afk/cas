//! Single-queue vhost-user block device backed by a regular file and io_uring.
mod backend;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::Parser;
use vhost_user_backend::VhostUserDaemon;
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
    .map_err(|e| std::io::Error::other(format!("{e:?}")))?;
    daemon.get_epoll_handlers()[0].register_listener(completion_fd, EventSet::IN, 2)?;
    let result = daemon.serve(&args.socket);
    drop(daemon);
    let mut backend = backend.lock().unwrap();
    let pending_at_disconnect = backend.pending_count();
    backend.drain()?;
    serde_json::to_writer_pretty(
        report,
        &backend.report(pending_at_disconnect, result.is_ok()),
    )?;
    result.map_err(|e| std::io::Error::other(format!("{e:?}")))?;
    Ok(())
}
