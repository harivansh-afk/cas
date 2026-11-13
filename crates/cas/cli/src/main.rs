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
    #[command(flatten)]
    Lab(cas_harness::lab::LabCommand),
    /// Count fixed 4/16 KiB content in immutable raw images; first image is the base.
    /// Zero chunks are excluded. Normalize guest free space before scanning.
    Census {
        #[arg(required = true, num_args = 1..)]
        images: Vec<PathBuf>,
    },
    /// Create a new test log, exercise FLUSH/reopen, and emit a JSON check result.
    /// The path must not exist; the file is retained for inspection.
    StagingCheck { path: PathBuf },
    /// Ingest a raw image offline as a new catalog image: WAL, FLUSH, compaction
    /// into the shared chunk store, a committed manifest, then catalog publication.
    /// No host may hold the root. Prints a JSON receipt; progress goes to stderr.
    Import {
        /// Storage root: an existing empty directory is initialized, an existing root is reused; a missing directory is refused.
        #[arg(long)]
        root: PathBuf,
        #[arg(long, value_parser = cas_core::id::parse)]
        store: [u8; 16],
        #[arg(long, value_parser = cas_core::id::parse)]
        image: [u8; 16],
        /// Raw image of whole 4 KiB blocks; its length becomes the image size.
        #[arg(long)]
        from: PathBuf,
        /// Fixed store chunk bytes (4096 or 16384) for a new root; must match an existing root.
        #[arg(long)]
        chunk_bytes: Option<usize>,
        /// WAL and chunk segment capacity for a new root.
        #[arg(long, default_value_t = cas_core::import::DEFAULT_SEGMENT_BYTES)]
        segment_bytes: u64,
        /// Input bytes staged between FLUSH/compaction/reclamation passes (multiple of 1 MiB).
        #[arg(long, default_value_t = cas_core::import::DEFAULT_COMPACTION_INTERVAL)]
        compact_every_bytes: u64,
        /// Metadata budget cap (bytes): rebuilt chunk index, WAL index and manifest scratch.
        #[arg(long, default_value_t = 4 << 30)]
        metadata_bytes: usize,
    },
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::Lab(command) => {
            cas_harness::lab::run(command)?;
            Ok(())
        }
        Command::Census { images } => {
            let results = cas_core::census::CHUNK_SIZES
                .into_iter()
                .map(|size| cas_core::census::scan(&images, size))
                .collect::<Result<Vec<_>, _>>()?;
            if results[0]
                .images
                .iter()
                .zip(&results[1].images)
                .any(|(first, second)| first.blake3 != second.blake3)
            {
                return Err("images changed between census passes".into());
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema_version": 1,
                    "hash": "BLAKE3-256",
                    "scope": "raw bytes; all-zero chunks excluded; no allocation or lineage inferred",
                    "results": results,
                }))?
            );
            Ok(())
        }
        Command::StagingCheck { path } => staging_check(path),
        Command::Import {
            root,
            store,
            image,
            from,
            chunk_bytes,
            segment_bytes,
            compact_every_bytes,
            metadata_bytes,
        } => import(
            root,
            store,
            image,
            from,
            chunk_bytes,
            segment_bytes,
            compact_every_bytes,
            metadata_bytes,
        ),
    }
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn import(
    root: PathBuf,
    store: [u8; 16],
    image: [u8; 16],
    from: PathBuf,
    chunk_bytes: Option<usize>,
    segment_bytes: u64,
    compact_every_bytes: u64,
    metadata_bytes: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    use cas_core::budget::{Amount, Budget};
    let chunk_size = chunk_bytes
        .map(cas_core::chunk::ChunkSize::from_bytes)
        .transpose()?;
    let budget = |bytes| Budget::new(Amount { bytes, requests: 0 });
    let mut printed = 0;
    let receipt = cas_core::import::run_with(
        cas_core::import::Request {
            root: &root,
            store,
            image,
            input: &from,
            chunk_size,
            segment_bytes,
            compaction_interval: compact_every_bytes,
        },
        budget(metadata_bytes),
        budget(256 * cas_core::MAX_REQUEST_BYTES),
        |point| {
            if let cas_core::import::Point::Settled { bytes } = point
                && bytes >> 30 > printed
            {
                printed = bytes >> 30;
                eprintln!("casctl: imported {printed} GiB");
            }
            Ok(())
        },
    )?;
    println!("{}", serde_json::to_string_pretty(&receipt)?);
    Ok(())
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
fn import(
    _: PathBuf,
    _: [u8; 16],
    _: [u8; 16],
    _: PathBuf,
    _: Option<usize>,
    _: u64,
    _: u64,
    _: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    Err("the O_DIRECT importer requires Linux".into())
}

#[cfg(target_os = "linux")]
fn staging_check(path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    use cas_core::BLOCK_SIZE;
    use cas_core::staging::StagingLog;

    let mut log = StagingLog::create(&path, (4 * BLOCK_SIZE) as u64)?;
    let durable_bytes = [0x41; BLOCK_SIZE];
    log.write(0, &durable_bytes)?;
    log.write(BLOCK_SIZE as u64, &[0x42; BLOCK_SIZE])?;
    log.zero(BLOCK_SIZE as u64, BLOCK_SIZE as u64)?;
    if log.zero(0, 0)?.is_some() {
        return Err("empty discard allocated a sequence".into());
    }
    let durable = log.flush()?;
    log.write(0, &[0x43; BLOCK_SIZE])?;
    drop(log);

    let reopened = StagingLog::open(&path)?;
    let status = reopened.status();
    if reopened.read(0, BLOCK_SIZE)? != durable_bytes
        || reopened.read(BLOCK_SIZE as u64, BLOCK_SIZE)? != [0; BLOCK_SIZE]
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

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("casctl: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
