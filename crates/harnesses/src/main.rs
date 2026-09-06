//! Development VM checks and host inventories. No paper gate is inferred here.
mod evidence;
mod host;
mod process;
mod vm;

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
    /// Boot the pinned guest, verify its IO, and retain all run evidence.
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
