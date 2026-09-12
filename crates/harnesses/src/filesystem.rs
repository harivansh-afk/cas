//! Buffered application IO with an independent deterministic restart oracle.
use crate::{evidence, process};
use clap::ValueEnum;
use std::{
    fs,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Clone, Copy, ValueEnum, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Write,
    Verify,
    Resume,
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
    sqlite: PathBuf,
    #[arg(long)]
    continued: bool,
}

const CONTINUED: usize = 4 * 1024 * 1024;
const CREATED: usize = 512 * 1024;
const APPENDED: usize = 256 * 1024;
const OVERWRITE: std::ops::Range<usize> = 4096..12288;

fn initial_byte(offset: usize, image: u8) -> u8 {
    ((offset * 31 + (offset >> 12) * 7 + image as usize * 19) % 251) as u8
}

fn sqlite(args: &Args, label: &str, sql: &str) -> io::Result<String> {
    let output = args.output.join(label);
    let mut command = Command::new(&args.sqlite);
    command
        .arg("-batch")
        .arg(args.root.join("application.db"))
        .arg(sql);
    let result = process::run_logged(&mut command, &output, Duration::from_secs(45))?;
    if result.exit_code != Some(0) || result.error.is_some() {
        return Err(io::Error::other(format!("SQLite {label} failed")));
    }
    fs::read_to_string(output.join("stdout.log"))
}

fn write(args: &Args) -> io::Result<()> {
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(args.root.join("created.tmp"))?;
    let bytes: Vec<_> = (0..CREATED)
        .map(|offset| initial_byte(offset, args.image))
        .collect();
    file.write_all(&bytes)?;
    file.sync_all()?;
    let append: Vec<_> = (CREATED..CREATED + APPENDED)
        .map(|offset| initial_byte(offset, args.image))
        .collect();
    file.write_all(&append)?;
    file.seek(SeekFrom::Start(OVERWRITE.start as u64))?;
    file.write_all(&vec![0xa5 + args.image; OVERWRITE.len()])?;
    file.sync_all()?;
    drop(file);
    fs::rename(args.root.join("created.tmp"), args.root.join("renamed.bin"))?;
    File::open(&args.root)?.sync_all()?;
    let mut deleted = File::options()
        .write(true)
        .create_new(true)
        .open(args.root.join("deleted.bin"))?;
    deleted.write_all(&bytes)?;
    deleted.sync_all()?;
    drop(deleted);
    fs::remove_file(args.root.join("deleted.bin"))?;
    File::open(&args.root)?.sync_all()?;
    sqlite(
        args,
        "sqlite-write",
        &format!(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE rows(id INTEGER PRIMARY KEY, value INTEGER NOT NULL); BEGIN; WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1024) INSERT INTO rows SELECT x,x*7+{} FROM n; UPDATE rows SET value=value+100 WHERE id%3=0; COMMIT; PRAGMA wal_checkpoint(TRUNCATE);",
            args.image
        ),
    )?;
    File::open(&args.root)?.sync_all()
}

fn verify(args: &Args) -> io::Result<serde_json::Value> {
    let file = args.root.join("renamed.bin");
    let mut actual = Vec::new();
    File::open(&file)?
        .take((CREATED + APPENDED + 1) as u64)
        .read_to_end(&mut actual)?;
    if actual.len() != CREATED + APPENDED
        || actual.iter().enumerate().any(|(offset, &byte)| {
            byte != if OVERWRITE.contains(&offset) {
                0xa5 + args.image
            } else {
                initial_byte(offset, args.image)
            }
        })
    {
        return Err(io::Error::other(
            "buffered file differs from restart oracle",
        ));
    }
    for name in ["created.tmp", "deleted.bin"] {
        match args.root.join(name).symlink_metadata() {
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
            Ok(_) => return Err(io::Error::other("rename or deletion was lost")),
        }
    }
    let database = sqlite(
        args,
        "sqlite-verify",
        &format!(
            "PRAGMA integrity_check; SELECT count(*),count(DISTINCT id),sum(value != id*7+{}+CASE WHEN id%3=0 THEN 100 ELSE 0 END),min(id),max(id) FROM rows;",
            args.image
        ),
    )?;
    if database != "ok\n1024|1024|0|1|1024\n" {
        return Err(io::Error::other("SQLite contents or integrity differ"));
    }
    let continued = if args.continued || matches!(args.phase, Phase::Resume) {
        let mut bytes = Vec::new();
        File::open(args.root.join("continued.bin"))?
            .take((CONTINUED + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() != CONTINUED
            || bytes
                .iter()
                .enumerate()
                .any(|(offset, &byte)| byte != initial_byte(offset, args.image + 7))
        {
            return Err(io::Error::other(
                "continuation file differs from restart oracle",
            ));
        }
        bytes.len()
    } else {
        0
    };
    Ok(
        serde_json::json!({"schema_version":1,"passed":true,"phase":args.phase,"image":args.image,
        "file_bytes":actual.len(),"file_blake3":blake3::hash(&actual).to_hex().to_string(),
        "sqlite_rows":1024,"sqlite_integrity":"ok","buffered":true,"continued_bytes":continued}),
    )
}

pub fn run(args: Args) -> io::Result<()> {
    fs::create_dir(&args.output)?;
    let result = (|| {
        require_ext4(&args.root)?;
        if matches!(args.phase, Phase::Write) {
            write(&args)?;
        }
        if matches!(args.phase, Phase::Resume) {
            let bytes: Vec<_> = (0..CONTINUED)
                .map(|offset| initial_byte(offset, args.image + 7))
                .collect();
            let mut file = File::options()
                .write(true)
                .create_new(true)
                .open(args.root.join("continued.bin"))?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            File::open(&args.root)?.sync_all()?;
        }
        verify(&args)
    })();
    let report = match &result {
        Ok(report) => report.clone(),
        Err(error) => {
            serde_json::json!({"schema_version":1,"passed":false,"phase":args.phase,"image":args.image,"error":error.to_string()})
        }
    };
    evidence::write_json(&args.output.join("filesystem.json"), &report)?;
    result.map(|_| ())
}

fn require_ext4(path: &Path) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let file = File::open(path)?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the directory FD remains open and stat is initialized only on success.
    if unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatfs filled the result structure.
    if unsafe { stat.assume_init() }.f_type != libc::EXT4_SUPER_MAGIC {
        return Err(io::Error::other("filesystem workload requires ext4"));
    }
    Ok(())
}
