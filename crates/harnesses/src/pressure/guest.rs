//! Direct checked reads, finite fio writes, and a fresh-boot content oracle.
use super::*;
use cas_core::{BLOCK_SIZE, aligned::AlignedBuffer};
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::{FileExt, OpenOptionsExt},
    path::PathBuf,
    process::Command,
    time::Instant,
};

#[derive(Clone, Copy, clap::ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Write,
    Verify,
}
#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, value_enum)]
    phase: Phase,
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=1))]
    image: u8,
    #[arg(long)]
    fio: PathBuf,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Digest {
    name: String,
    bytes: u64,
    blake3: String,
}

fn seed(bytes: &mut [u8], domain: u64, block: u64) {
    let mut hash = blake3::Hasher::new();
    hash.update(b"cas-pressure-v1");
    hash.update(&domain.to_le_bytes());
    hash.update(&block.to_le_bytes());
    hash.finalize_xof().fill(bytes);
}
fn direct(path: &Path, write: bool) -> io::Result<File> {
    let mut options = File::options();
    options.read(true).custom_flags(libc::O_DIRECT);
    if write {
        options.write(true).create_new(true);
    }
    options.open(path)
}
fn write_seed(path: &Path, domain: u64, bytes: u64, rate: Option<u64>) -> io::Result<Duration> {
    let mut file = direct(path, true)?;
    // Large writes expose guest request splitting without another whole-file buffer.
    let mut buffer = AlignedBuffer::new(1024 * 1024);
    let start = Instant::now();
    for offset in (0..bytes).step_by(buffer.as_slice().len()) {
        process::check_interrupt()?;
        for (index, block) in buffer
            .as_mut_slice()
            .as_chunks_mut::<BLOCK_SIZE>()
            .0
            .iter_mut()
            .enumerate()
        {
            seed(block, domain, offset / BLOCK_SIZE as u64 + index as u64);
        }
        file.write_all(buffer.as_slice())?;
        if let Some(rate) = rate {
            let due = Duration::from_secs_f64(
                (offset + buffer.as_slice().len() as u64) as f64 / rate as f64,
            );
            while start.elapsed() < due {
                process::check_interrupt()?;
                std::thread::sleep(due.saturating_sub(start.elapsed()).min(process::POLL));
            }
        }
        if start.elapsed() > COMMAND_TIMEOUT {
            return Err(io::ErrorKind::TimedOut.into());
        }
    }
    file.sync_all()?;
    Ok(start.elapsed())
}
fn checked_reads(path: &Path, domain: u64, stage: Stage, image: u8) -> io::Result<Completed> {
    let file = direct(path, false)?;
    let mut buffer = AlignedBuffer::new(BLOCK_SIZE);
    let mut expected = [0; BLOCK_SIZE];
    let mut latency = Latency::default();
    let start = Instant::now();
    let blocks = if stage == Stage::ScanHot && image == 1 {
        1024 * 1024
    } else {
        SET_BYTES
    } / BLOCK_SIZE as u64;
    let operations = if stage == Stage::SharedMiss {
        1
    } else {
        2 * blocks
    };
    loop {
        let block = latency.count % blocks;
        let before = Instant::now();
        file.read_exact_at(buffer.as_mut_slice(), block * BLOCK_SIZE as u64)?;
        latency.record(before.elapsed());
        seed(&mut expected, domain, block);
        if buffer.as_slice() != expected {
            return Err(io::Error::other(
                "direct read differs from deterministic seed",
            ));
        }
        process::check_interrupt()?;
        if stage == Stage::ScanHot {
            if start.elapsed() >= Duration::from_secs(4) {
                break;
            }
        } else if latency.count >= operations {
            break;
        }
        if start.elapsed() >= COMMAND_TIMEOUT {
            return Err(io::ErrorKind::TimedOut.into());
        }
    }
    Ok(Completed {
        stage,
        image,
        bytes: latency.count * BLOCK_SIZE as u64,
        elapsed_ns: start.elapsed().as_nanos() as u64,
        read_latency: Some(latency),
    })
}
fn fio(args: &Args, directory: &Path, data: &Path, stage: Stage) -> io::Result<Completed> {
    let control = stage.fio().expect("fio stage");
    let mut command = Command::new(&args.fio);
    command
        .arg(format!("--name={}", stage.name()))
        .arg(format!("--filename={}", data.display()))
        .args([
            "--rw=write",
            "--direct=1",
            "--ioengine=io_uring",
            "--verify=crc32c",
            "--verify_fatal=1",
            "--verify_state_save=0",
            "--do_verify=1",
            "--end_fsync=1",
            "--randrepeat=1",
            "--refill_buffers=1",
            "--output-format=json+",
        ])
        .arg(format!("--size={}", control.size_bytes))
        .arg(format!("--bs={}", control.block_bytes))
        .arg(format!("--iodepth={}", control.depth))
        .arg(format!("--output={}", directory.join("fio.json").display()));
    let start = Instant::now();
    let result = process::run_logged(&mut command, &directory.join("command"), COMMAND_TIMEOUT)?;
    if result.exit_code != Some(0) || result.error.is_some() {
        return Err(io::Error::other("pressure fio command failed"));
    }
    evidence::read_json::<evidence::Fio>(&directory.join("fio.json"))?.verify(
        stage.name(),
        control.size_bytes,
        control.size_bytes,
    )?;
    Ok(Completed {
        stage,
        image: args.image,
        bytes: control.size_bytes,
        elapsed_ns: start.elapsed().as_nanos() as u64,
        read_latency: None,
    })
}
fn digest(path: &Path) -> io::Result<Digest> {
    let mut file = File::open(path)?;
    let mut buffer = [0; 64 * 1024];
    let mut hash = blake3::Hasher::new();
    let mut bytes = 0;
    loop {
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
        bytes += size as u64;
    }
    Ok(Digest {
        name: path.file_name().unwrap().to_string_lossy().into_owned(),
        bytes,
        blake3: hash.finalize().to_hex().to_string(),
    })
}
fn verify_seed(path: &Path, domain: u64, bytes: u64) -> io::Result<()> {
    let file = direct(path, false)?;
    if file.metadata()?.len() != bytes {
        return Err(io::Error::other("seed length differs"));
    }
    let mut actual = AlignedBuffer::new(BLOCK_SIZE);
    let mut expected = [0; BLOCK_SIZE];
    for block in 0..bytes / BLOCK_SIZE as u64 {
        file.read_exact_at(actual.as_mut_slice(), block * BLOCK_SIZE as u64)?;
        seed(&mut expected, domain, block);
        if actual.as_slice() != expected {
            return Err(io::Error::other("restart seed differs"));
        }
    }
    Ok(())
}
fn names() -> Vec<String> {
    ["shared".to_owned(), "private".to_owned()]
        .into_iter()
        .chain(
            Stage::ALL
                .into_iter()
                .filter(|stage| {
                    stage.fio().is_some() || matches!(stage, Stage::Calibration | Stage::Burst)
                })
                .map(|stage| stage.name().to_owned()),
        )
        .collect()
}
fn verify(args: &Args, root: &Path) -> io::Result<Vec<Digest>> {
    verify_seed(&root.join("shared"), 0, SET_BYTES)?;
    verify_seed(&root.join("private"), 1 + args.image as u64, SET_BYTES)?;
    verify_seed(
        &root.join("calibration"),
        10 + args.image as u64,
        WRITE_BYTES,
    )?;
    verify_seed(&root.join("burst"), 20 + args.image as u64, WRITE_BYTES)?;
    names()
        .iter()
        .map(|name| digest(&root.join(name)))
        .collect()
}
fn write(args: &Args, root: &Path) -> io::Result<Vec<Digest>> {
    fs::create_dir(root)?;
    write_seed(&root.join("shared"), 0, SET_BYTES, None)?;
    write_seed(
        &root.join("private"),
        1 + args.image as u64,
        SET_BYTES,
        None,
    )?;
    for stage in Stage::ALL {
        let directory = args.output.join(stage.name());
        fs::create_dir(&directory)?;
        publish(
            &directory.join("ready.json"),
            &serde_json::json!({"image": args.image, "stage": stage}),
        )?;
        wait(&directory.join("request.json"))?;
        let request: Request = evidence::read_json(&directory.join("request.json"))?;
        if request.stage != stage
            || (stage == Stage::Burst) != request.rate_bytes_per_second.is_some()
            || request.rate_bytes_per_second == Some(0)
        {
            return Err(io::Error::other(
                "pressure stage request differs from inventory",
            ));
        }
        let result = if stage.fio().is_some() {
            fio(args, &directory, &root.join(stage.name()), stage)?
        } else if matches!(stage, Stage::Calibration | Stage::Burst) {
            let domain = if stage == Stage::Calibration { 10 } else { 20 } + args.image as u64;
            let elapsed = write_seed(
                &root.join(stage.name()),
                domain,
                WRITE_BYTES,
                request.rate_bytes_per_second,
            )?;
            Completed {
                stage,
                image: args.image,
                bytes: WRITE_BYTES,
                elapsed_ns: elapsed.as_nanos() as u64,
                read_latency: None,
            }
        } else {
            let private = matches!(stage, Stage::Disjoint | Stage::Displace)
                || (stage == Stage::ScanHot && args.image == 0);
            checked_reads(
                &root.join(if private { "private" } else { "shared" }),
                if private { 1 + args.image as u64 } else { 0 },
                stage,
                args.image,
            )?
        };
        publish(&directory.join("completed.json"), &result)?;
    }
    // Stay attached until the host controller has observed idle D == E.
    wait(&args.output.join("finish.json"))?;
    let digests = verify(args, root)?;
    evidence::write_json(&root.join("digests.json"), &digests)?;
    File::open(root.join("digests.json"))?.sync_all()?;
    File::open(root)?.sync_all()?;
    Ok(digests)
}
pub fn run(args: Args) -> io::Result<()> {
    fs::create_dir(&args.output)?;
    let root = args.root.join("pressure");
    let started = crate::host::utc_now()?;
    let result = match args.phase {
        Phase::Write => write(&args, &root),
        Phase::Verify => (|| {
            let actual = verify(&args, &root)?;
            let expected: Vec<Digest> = evidence::read_json(&root.join("digests.json"))?;
            if serde_json::to_value(&actual)? != serde_json::to_value(&expected)? {
                return Err(io::Error::other("fresh-boot pressure digests differ"));
            }
            Ok(actual)
        })(),
    };
    evidence::write_json(
        &args.output.join("pressure.json"),
        &serde_json::json!({
            "schema_version":1, "passed":result.is_ok(), "phase":args.phase, "image":args.image,
            "started_at_utc":started, "ended_at_utc":crate::host::utc_now()?,
            "files":result.as_ref().ok(), "error":result.as_ref().err().map(ToString::to_string),
        }),
    )?;
    result.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direct_oracle_rejects_wrong_identity_corruption_and_truncation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("seed");
        let bytes = 1024 * 1024;
        write_seed(&path, 7, bytes, None).unwrap();
        verify_seed(&path, 7, bytes).unwrap();
        assert!(verify_seed(&path, 8, bytes).is_err());
        let file = File::options().write(true).open(&path).unwrap();
        file.write_all_at(&[0xff], 17).unwrap();
        file.sync_all().unwrap();
        assert!(verify_seed(&path, 7, bytes).is_err());
        file.set_len(bytes - 4096).unwrap();
        assert!(verify_seed(&path, 7, bytes).is_err());
    }
}
