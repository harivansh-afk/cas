use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about = "CAS research implementation tools")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new test log, exercise FLUSH/reopen, and emit a JSON check result.
    /// The path must not exist; the file is retained for inspection.
    StagingCheck { path: PathBuf },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::StagingCheck { path } => staging_check(path),
    }
}

#[cfg(target_os = "linux")]
fn staging_check(path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    use cas_core::BLOCK_SIZE;
    use cas_core::staging::StagingLog;

    let mut log = StagingLog::create(&path, (4 * BLOCK_SIZE) as u64)?;
    let durable_bytes = vec![0x41; BLOCK_SIZE];
    log.write(0, &durable_bytes)?;
    log.write(BLOCK_SIZE as u64, &vec![0x42; BLOCK_SIZE])?;
    log.zero(BLOCK_SIZE as u64, BLOCK_SIZE as u64)?;
    if log.zero(0, 0)?.is_some() {
        return Err("empty discard allocated a sequence".into());
    }
    let durable = log.flush()?;
    log.write(0, &vec![0x43; BLOCK_SIZE])?;
    drop(log);

    let reopened = StagingLog::open(&path)?;
    let status = reopened.status();
    if reopened.read(0, BLOCK_SIZE)? != durable_bytes
        || reopened.read(BLOCK_SIZE as u64, BLOCK_SIZE)? != vec![0; BLOCK_SIZE]
        || status.durable != durable
    {
        return Err("staging recovery did not reproduce the flushed image".into());
    }
    let path = serde_json::to_value(path)?;
    println!(
        "{:#}",
        serde_json::json!({
            "schema_version": 1,
            "check": "staging_flush_reopen",
            "passed": true,
            "path": path,
            "io": "O_DIRECT",
            "implementation": "synchronous_reference",
            "durable_sequence": status.durable,
            "log_bytes": status.log_bytes,
            "discarded_unflushed_tail_bytes": status.recovered_tail_bytes,
            "paper_gate": null,
        })
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn staging_check(_: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    Err("the O_DIRECT staging check requires Linux".into())
}
