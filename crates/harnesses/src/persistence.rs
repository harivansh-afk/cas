//! Deterministic persistence schedules, with an independent logical image oracle.
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use cas_core::aligned::AlignedBuffer;
use cas_core::append::{
    self, Log,
    format::{Builder, RequestId},
};
use cas_core::{BLOCK_SIZE, MAX_REQUEST_BYTES};
use serde::Serialize;

use crate::{evidence, host, process, source};

mod oracle;
mod schedule;
mod verify;
use oracle::{Operation, Oracle};
use schedule::{Schedule, schedules};
pub use verify::verify;

const IMAGE_BYTES: usize = 16 * BLOCK_SIZE;
const SECTOR_BYTES: usize = 512;
const SEED: u64 = 0x4341_5350_4f57_4552;
type Files = BTreeMap<String, Vec<u8>>;

#[derive(clap::Args)]
pub struct Args {
    /// New evidence directory; prior attempts are never overwritten.
    #[arg(long)]
    output: PathBuf,
}

fn read_files(path: &Path) -> io::Result<Files> {
    fs::read_dir(path)?
        .map(|entry| {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                return Err(io::Error::other("unexpected fixture entry"));
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| io::Error::other("non-UTF8 fixture name"))?;
            Ok((name, fs::read(entry.path())?))
        })
        .collect()
}

fn write_files(path: &Path, files: &Files) -> io::Result<()> {
    fs::create_dir(path)?;
    for (name, bytes) in files {
        fs::write(path.join(name), bytes)?;
    }
    Ok(())
}

struct Window {
    durable: Files,
    final_files: Files,
    tail_file: String,
    required: u64,
    oracle: Oracle,
    headers: Vec<usize>,
}

impl Window {
    fn create(path: &Path, rollover: bool, sync_tail: bool) -> io::Result<Self> {
        let config = append::Config {
            store: [0x31; 16],
            image: [0x72; 16],
            image_bytes: IMAGE_BYTES as u64,
            segment_bytes: (2 * MAX_REQUEST_BYTES) as u64,
        };
        let mut log =
            Log::create(path, config, append::Limits::default()).map_err(io::Error::other)?;
        let mut oracle = Oracle::new();
        oracle.append(
            &mut log,
            &[
                Operation {
                    block: 0,
                    blocks: 4,
                    seed: Some(11),
                },
                Operation {
                    block: 4,
                    blocks: 4,
                    seed: Some(29),
                },
            ],
        )?;
        let mut required = log.flush().map_err(io::Error::other)?;
        if required != 2 {
            return Err(io::Error::other(
                "fixture did not synchronize its required prefix",
            ));
        }
        if rollover {
            log.rollover().map_err(io::Error::other)?;
        }
        let mut durable = read_files(path)?;
        let tail_file = durable.keys().next_back().unwrap().clone();
        let mut headers = Vec::new();
        let mut tail_bytes = 0;
        for operations in [
            &[
                Operation {
                    block: 1,
                    blocks: 2,
                    seed: Some(51),
                },
                Operation {
                    block: 6,
                    blocks: 2,
                    seed: None,
                },
                Operation {
                    block: 8,
                    blocks: 1,
                    seed: Some(71),
                },
            ][..],
            &[
                Operation {
                    block: 0,
                    blocks: 2,
                    seed: Some(91),
                },
                Operation {
                    block: 9,
                    blocks: 3,
                    seed: Some(113),
                },
            ][..],
            &[
                Operation {
                    block: 0,
                    blocks: 4,
                    seed: None,
                },
                Operation {
                    block: 2,
                    blocks: 4,
                    seed: Some(137),
                },
            ][..],
        ] {
            headers.extend(
                (tail_bytes..tail_bytes + BLOCK_SIZE)
                    .step_by(SECTOR_BYTES)
                    .map(|offset| offset / SECTOR_BYTES),
            );
            tail_bytes += oracle.append(&mut log, operations)?;
        }
        headers.extend(
            (tail_bytes..tail_bytes + BLOCK_SIZE)
                .step_by(SECTOR_BYTES)
                .map(|offset| offset / SECTOR_BYTES),
        );
        // Persist the FENCE write without its fdatasync: a FENCE alone is not E.
        let fence = log.prepare_fence().map_err(io::Error::other)?;
        fence.write()?;
        if log.status().durable != required {
            return Err(io::Error::other("unsynced fence advanced E"));
        }
        if sync_tail {
            fence.file().sync_data()?;
            required = log.complete_sync(&fence).map_err(io::Error::other)?;
            durable = read_files(path)?;
            headers.clear();
        }
        drop(fence);
        drop(log);
        let final_files = read_files(path)?;
        Ok(Self {
            durable,
            final_files,
            tail_file,
            required,
            oracle,
            headers,
        })
    }

    fn materialize(&self, schedule: &Schedule) -> io::Result<Files> {
        let mut files = self.durable.clone();
        let stable = self.durable[&self.tail_file].len();
        let final_bytes = &self.final_files[&self.tail_file];
        let bytes = files.get_mut(&self.tail_file).unwrap();
        bytes.resize(stable + schedule.tail_length, 0);
        for &sector in &schedule.persisted_sectors {
            let start = stable + sector * SECTOR_BYTES;
            let end = (start + SECTOR_BYTES).min(bytes.len());
            if start >= bytes.len() || end > final_bytes.len() {
                return Err(io::Error::other("schedule exceeds its unsynced tail"));
            }
            bytes[start..end].copy_from_slice(&final_bytes[start..end]);
        }
        for (name, durable) in &self.durable {
            if !files[name].starts_with(durable) {
                return Err(io::Error::other("model changed successfully synced bytes"));
            }
        }
        Ok(files)
    }

    fn exercise(&self, directory: &Path, schedule: &Schedule) -> io::Result<()> {
        process::check_interrupt()?;
        fs::create_dir(directory)?;
        evidence::write_json(&directory.join("schedule.json"), schedule)?;
        let files = self.materialize(schedule)?;
        write_files(&directory.join("input"), &files)?;
        write_files(&directory.join("recovered"), &files)?;
        let result = (|| {
            // No expected-prefix argument feeds the recovery decision. The
            // independent oracle checks the result after ordinary cold recovery.
            let mut log = Log::open(directory.join("recovered"), append::Limits::default())
                .map_err(io::Error::other)?;
            let mut bytes = AlignedBuffer::new(IMAGE_BYTES);
            log.read_into(0, &mut bytes).map_err(io::Error::other)?;
            fs::write(directory.join("observed.bin"), bytes.as_slice())?;
            let status = log.status();
            self.oracle
                .verify(status.published, self.required, bytes.as_slice())?;
            if status.durable != status.published {
                return Err(io::Error::other(
                    "cold recovery did not synchronize its retained prefix",
                ));
            }
            Ok::<_, io::Error>(status)
        })();
        evidence::write_json(
            &directory.join("result.json"),
            &serde_json::json!({
                "passed": result.is_ok(),
                "required_prefix": self.required,
                "status": result.as_ref().ok(),
                "error": result.as_ref().err().map(ToString::to_string),
                "artifacts": source::scan(directory)?
            }),
        )?;
        result.map(|_| ())
    }

    fn negative_controls(&self, directory: &Path) -> io::Result<()> {
        fs::create_dir(directory)?;
        let mut files = self.durable.clone();
        // The first batch begins after the segment header; its payload follows
        // the batch header. Damage acknowledged bytes, outside the crash model.
        let first = files.keys().next().unwrap().clone();
        files.get_mut(&first).unwrap()[2 * BLOCK_SIZE] ^= 1;
        let path = directory.join("corrupt-required");
        write_files(&path, &files)?;
        let before = source::scan(&path)?;
        if !matches!(
            Log::open_with_expected_prefix(&path, append::Limits::default(), self.required),
            Err(append::Error::Prefix { .. })
        ) {
            return Err(io::Error::other(
                "required durable corruption negative control passed",
            ));
        }
        source::compare(
            &before,
            &source::scan(&path)?,
            "failed recovery changed corrupt evidence",
        )?;
        let mut wrong = self.oracle.images[&self.required].clone();
        wrong[0] ^= 1;
        if self
            .oracle
            .verify(self.required, self.required, &wrong)
            .is_ok()
            || self
                .oracle
                .verify(self.required - 1, self.required, &wrong)
                .is_ok()
            || self.oracle.verify(3, 2, &self.oracle.images[&5]).is_ok()
        {
            return Err(io::Error::other(
                "wrong image or missing prefix negative control passed",
            ));
        }
        evidence::write_json(
            &directory.join("result.json"),
            &serde_json::json!({
                "passed": true,
                "required_corruption_refused_before_repair": true,
                "wrong_image_rejected": true,
                "missing_prefix_rejected": true,
                "partial_batch_rejected": true
            }),
        )
    }
}

fn execute(output: &Path) -> io::Result<usize> {
    let mut cases = 0;
    for (name, rollover, sync_tail) in [
        ("same-segment", false, false),
        ("after-rollover", true, false),
        ("after-sync", false, true),
    ] {
        let directory = output.join(name);
        fs::create_dir(&directory)?;
        let window = Window::create(&directory.join("encoder-output"), rollover, sync_tail)?;
        write_files(&directory.join("durable-input"), &window.durable)?;
        evidence::write_json(
            &directory.join("operations.json"),
            &window.oracle.operations,
        )?;
        let oracles = directory.join("oracles");
        fs::create_dir(&oracles)?;
        for (prefix, bytes) in &window.oracle.images {
            fs::write(oracles.join(format!("prefix-{prefix}.bin")), bytes)?;
        }
        window.negative_controls(&directory.join("negative-controls"))?;
        let tail =
            window.final_files[&window.tail_file].len() - window.durable[&window.tail_file].len();
        evidence::write_json(
            &directory.join("window.json"),
            &serde_json::json!({
                "required_prefix":window.required, "issued_prefix":window.oracle.sequence,
                "tail_file":window.tail_file, "tail_bytes":tail, "header_sectors":window.headers,
                "complete_batch_prefixes":window.oracle.images.keys().collect::<Vec<_>>()
            }),
        )?;
        for schedule in schedules(tail / SECTOR_BYTES, &window.headers) {
            window.exercise(&directory.join(&schedule.id), &schedule)?;
            cases += 1;
        }
    }
    Ok(cases)
}

pub fn run(args: Args) -> io::Result<()> {
    fs::create_dir(&args.output)?;
    let started = host::utc_now()?;
    let result = execute(&args.output);
    evidence::write_json(
        &args.output.join("model.json"),
        &serde_json::json!({
            "schema_version":1, "model":"synced-prefix-unsynced-sector-schedules-v1", "seed":SEED,
            "sector_bytes":SECTOR_BYTES, "started_at_utc":started, "ended_at_utc":host::utc_now()?,
            "passed":result.is_ok(), "cases":result.as_ref().ok(), "error":result.as_ref().err().map(ToString::to_string),
            "binary":source::entry(&std::env::current_exe()?)?, "artifacts":source::scan(&args.output)?,
            "paper_gates":[], "physical_power_loss_tested":false
        }),
    )?;
    result?;
    verify(&args.output)
}
