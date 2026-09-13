//! Development VM checks and host inventories. No paper gate is inferred here.
use cas_harness::{
    filesystem, fixture, fleet, host, persistence, pressure, process, shared, suite, vm,
};

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about = "CAS development tests and host checks")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the source-bound development checkpoint suite from a Nix wrapper.
    Suite(suite::Args),
    /// Exercise buffered files and SQLite on a mounted ext4 guest disk.
    Filesystem(filesystem::Args),
    /// Run the fixed development pressure controls inside one ext4 guest.
    Pressure(pressure::Args),
    /// Own both inner filesystem guests and the shared host inside the XFS fixture.
    Shared(shared::Args),
    /// Run native storage IO and real reflink controls on a fresh XFS KVM guest.
    Fixture(fixture::Args),
    /// Verify every retained XFS fixture assertion and artifact.
    VerifyFixture {
        #[arg(long)]
        output: PathBuf,
    },
    /// Recheck a retained suite, including every evidence hash and required scenario.
    VerifySuite {
        #[arg(long)]
        output: PathBuf,
    },
    /// Recover real WAL bytes under deterministic unsynced-tail persistence schedules.
    Persistence(persistence::Args),
    /// Recheck every mandatory persistence case and retained artifact.
    VerifyPersistence {
        #[arg(long)]
        output: PathBuf,
    },
    /// Build a dated ARM64 clone fleet and census each update epoch.
    Fleet(fleet::Args),
    /// Boot the pinned guest for automated checks or an interactive SSH session.
    Vm(vm::Args),
    /// Capture host settings and tool versions without running a benchmark.
    Preflight {
        #[arg(long)]
        label: String,
        #[arg(long)]
        output: PathBuf,
        /// Checkout to inventory; defaults to the current directory.
        #[arg(long, default_value = ".")]
        checkout: PathBuf,
    },
    /// Check that two stable paths identify distinct whole disks; never writes.
    CheckDisks {
        os_disk: PathBuf,
        data_disk: PathBuf,
    },
}

fn run() -> io::Result<()> {
    let args = Args::parse();
    process::install_signal_handlers()?;
    match args.command {
        Command::Suite(args) => suite::run(args),
        Command::Filesystem(args) => filesystem::run(args),
        Command::Pressure(args) => pressure::run(args),
        Command::Shared(args) => shared::run(args),
        Command::Fixture(args) => fixture::run(args),
        Command::VerifyFixture { output } => fixture::verify(&output),
        Command::VerifySuite { output } => suite::verify(&output),
        Command::Persistence(args) => persistence::run(args),
        Command::VerifyPersistence { output } => persistence::verify(&output),
        Command::Fleet(args) => fleet::run(args),
        Command::Vm(args) => vm::run(args),
        Command::Preflight {
            label,
            output,
            checkout,
        } => host::preflight(&label, &output, &checkout),
        Command::CheckDisks { os_disk, data_disk } => {
            host::check_disks(&os_disk, &data_disk)?;
            println!("OS and experiment paths identify distinct whole block devices.");
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cas-harness: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_options_do_not_change_smoke_defaults_or_combine_with_recovery() {
        let parse = |extra: &[&str]| {
            Args::try_parse_from(
                [
                    "cas-harness",
                    "vm",
                    "--output",
                    "run",
                    "--vm",
                    "vm",
                    "--build-info",
                    "build",
                    "--lock",
                    "lock",
                ]
                .into_iter()
                .chain(extra.iter().copied()),
            )
        };
        assert!(parse(&[]).is_ok());
        assert!(parse(&["--live-recovery", "--crash-at", "after-prepared"]).is_ok());
        assert!(
            parse(&[
                "--live-recovery",
                "--crash-at",
                "before-submit",
                "--replay-crash-at",
                "after-replay-append",
                "--replay-restarts",
                "2",
            ])
            .is_ok()
        );
        assert!(parse(&["--replay-crash-at", "after-replay-append"]).is_err());
        assert!(parse(&["--replay-restarts", "2"]).is_err());
        assert!(
            parse(&[
                "--live-recovery",
                "--replay-crash-at",
                "after-replay-append",
                "--replay-restarts",
                "4",
            ])
            .is_err()
        );
        assert!(parse(&["--device-reset"]).is_ok());
        for other in ["--recovery", "--live-recovery"] {
            assert!(parse(&["--device-reset", other]).is_err());
        }
        assert!(parse(&["--device-reset", "--ssh-key", "id.pub"]).is_err());
        assert!(parse(&["--ssh-key", "id.pub"]).is_ok());
        assert!(parse(&["--ssh-key", "id.pub", "--ssh-port", "23480"]).is_ok());
        for args in [
            vec!["--ssh-port", "23480"],
            vec!["--ssh-key", "id.pub", "--ssh-port", "0"],
            vec!["--ssh-key", "id.pub", "--recovery"],
            vec!["--ssh-key", "id.pub", "--live-recovery"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }
}
