// Vhost-user block device with reference storage and a concurrent local adapter.

use cas_daemon::{BackendKind, backend, fault};

use std::io;
use std::num::NonZeroU64;
use std::path::PathBuf;

use clap::Parser;

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
    cas_daemon::service::serve(backend, &args.socket, report)?;
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
