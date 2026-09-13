//! Named local VM experiments. Systemd owns each lab; the existing harness owns its children.
mod bench;
mod client;
mod runtime;
mod samples;

use crate::{evidence, process::ManagedChild};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    fs::File,
    io,
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const GIB: u64 = 1024 * 1024 * 1024;
const IMAGE_BYTES: u64 = 512 * 1024 * 1024;
const SEGMENT_BYTES: u64 = 8 * 1024 * 1024;
const STORE: &str = "01010101010101010101010101010101";
const STARTUP: Duration = Duration::from_secs(100);

#[derive(Clone, Copy, Debug, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    Cas,
    Raw,
    Daemon,
}
impl Backend {
    fn name(self) -> &'static str {
        match self {
            Self::Cas => "cas",
            Self::Raw => "raw",
            Self::Daemon => "daemon",
        }
    }
}

#[derive(clap::Subcommand)]
pub enum Command {
    /// Create and boot a named CAS VM. --count creates peers in the same store.
    New {
        name: Option<String>,
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=4))]
        count: u8,
        /// Storage comparison arm; all arms use the same guest geometry.
        #[arg(long, value_enum, default_value_t = Backend::Cas)]
        backend: Backend,
        #[arg(long)]
        json: bool,
    },
    /// List labs, their guest names, state and disk allocation.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Open SSH, or execute a command after --. Peers are NAME/1, NAME/2, ...
    #[command(visible_alias = "ssh")]
    Shell {
        name: String,
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Boot a stopped lab with its existing data disks.
    Start { name: String },
    /// Shut down guests and storage, preserving disks and reports.
    Stop {
        name: String,
        #[arg(long)]
        force: bool,
    },
    /// Delete a stopped lab's disk and temporary VM data; archive small evidence.
    Rm { name: String },
    /// Show current storage counters and measured process memory.
    Status {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Show the latest supervisor, storage and guest logs.
    Logs { name: String },
    /// Check KVM, user services, build availability, RAM and disk headroom.
    Doctor,
    /// Measure one guest through fio; retain exact jobs, results and environment.
    Bench {
        name: String,
        #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u8).range(1..=10))]
        repeats: u8,
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u8).range(1..=30))]
        seconds: u8,
        #[arg(long, value_enum)]
        case: Option<bench::Case>,
    },
    #[command(hide = true)]
    LabSupervise { directory: PathBuf, run: PathBuf },
    #[command(hide = true)]
    LabHost { build: PathBuf },
}

#[derive(Clone, Deserialize, Serialize)]
struct Build {
    vm: PathBuf,
    source_revision: String,
    source_path: PathBuf,
    kernel: String,
    system: String,
}
#[derive(Clone, Deserialize, Serialize)]
struct Config {
    schema_version: u32,
    name: String,
    count: u8,
    backend: Backend,
    disk_bytes: u64,
    build: Build,
    unit: String,
}
#[derive(Deserialize, Serialize)]
struct Active {
    run: PathBuf,
    port: u16,
}

pub fn run(command: Command) -> io::Result<()> {
    match command {
        Command::New {
            name,
            count,
            backend,
            json,
        } => client::new(name, count, backend, json),
        Command::Ls { json } => client::list(json),
        Command::Shell { name, command } => {
            let status = client::ssh(&name, &command)?.status()?;
            if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!("SSH exited with {status}")))
            }
        }
        Command::Start { name } => client::start(&name),
        Command::Stop { name, force } => client::stop(&name, force),
        Command::Rm { name } => client::remove(&name),
        Command::Status { name, json } => client::status(&name, json),
        Command::Logs { name } => client::logs(&name),
        Command::Doctor => client::doctor(),
        Command::Bench {
            name,
            repeats,
            seconds,
            case,
        } => bench::run(&name, repeats, seconds, case),
        Command::LabSupervise { directory, run } => runtime::supervise(&directory, &run),
        Command::LabHost { build } => runtime::host(&build),
    }
}

fn timestamp() -> io::Result<String> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos()
        .to_string())
}
fn valid_name(name: &str) -> io::Result<()> {
    if name.is_empty()
        || name.len() > 40
        || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        || !name.as_bytes()[0].is_ascii_alphanumeric()
    {
        return Err(io::Error::other(
            "use a name of 1–40 letters, digits or hyphens, starting with a letter or digit",
        ));
    }
    Ok(())
}
fn root() -> io::Result<PathBuf> {
    let path = if let Some(path) = std::env::var_os("CASCTL_STATE_DIR") {
        PathBuf::from(path)
    } else if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        PathBuf::from(path).join("casctl")
    } else {
        PathBuf::from(
            std::env::var_os("HOME")
                .ok_or_else(|| io::Error::other("HOME or CASCTL_STATE_DIR is required"))?,
        )
        .join(".local/state/casctl")
    };
    fs::create_dir_all(&path)?;
    fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let path = path.canonicalize()?;
    if path
        .to_string_lossy()
        .contains([',', '\n', '\r', '"', '\\'])
    {
        return Err(io::Error::other(
            "state path contains a QEMU-incompatible character",
        ));
    }
    fs::create_dir_all(path.join("labs"))?;
    Ok(path)
}
fn lab(name: &str) -> io::Result<PathBuf> {
    valid_name(name)?;
    Ok(root()?.join("labs").join(name))
}
fn load(name: &str) -> io::Result<(PathBuf, Config)> {
    let dir = lab(name)?;
    let config = evidence::read_json(&dir.join("config.json"))?;
    Ok((dir, config))
}
fn publish(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut file = File::create(&tmp)?;
    evidence::write_json_to(&mut file, value)?;
    file.sync_all()?;
    fs::rename(tmp, path)?;
    File::open(
        path.parent()
            .ok_or_else(|| io::Error::other("missing parent"))?,
    )?
    .sync_all()
}
fn checked(command: &mut Process) -> io::Result<()> {
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{} exited with {status}",
            command.get_program().to_string_lossy()
        )))
    }
}
fn spawn(command: &mut Process, log: &Path) -> io::Result<ManagedChild> {
    let file = File::options().create_new(true).write(true).open(log)?;
    command.stdout(file.try_clone()?).stderr(file);
    ManagedChild::spawn(command)
}
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn image(index: u8) -> String {
    format!("{:032x}", u128::from(index) + 2)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_cannot_escape_owned_state_or_become_options() {
        for name in ["", "..", "a/b", "--all", "a\nb", "a b"] {
            assert!(valid_name(name).is_err());
        }
        assert!(valid_name("demo-1").is_ok());
        assert_eq!(quote("a'$(x)"), "'a'\\''$(x)'");
    }
}
