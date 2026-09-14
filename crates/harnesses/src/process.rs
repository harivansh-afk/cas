//! Child process groups with deadlines and cleanup on every return path.
use std::fs::File;
use std::io::{self, Read, Seek};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
pub const POLL: Duration = Duration::from_millis(20);

extern "C" fn interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

pub fn install_signal_handlers() -> io::Result<()> {
    // SAFETY: sigaction is initialized, the handler only stores to a lock-free
    // atomic, and no pointer retained by the kernel refers to stack storage.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = interrupt as *const () as libc::sighandler_t;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [libc::SIGINT, libc::SIGTERM] {
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

pub fn check_interrupt() -> io::Result<()> {
    if INTERRUPTED.load(Ordering::Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "test interrupted",
        ))
    } else {
        Ok(())
    }
}

pub fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| -status.signal().unwrap_or(1))
}

/// Send stdout and stderr to a new log file; an existing log is never overwritten.
pub fn log_to(command: &mut Command, log: &Path) -> io::Result<()> {
    let file = File::options().write(true).create_new(true).open(log)?;
    command.stdout(file.try_clone()?).stderr(file);
    Ok(())
}

/// Spawn an owned child whose output lands in a new log file.
pub fn spawn_logged(command: &mut Command, log: &Path) -> io::Result<ManagedChild> {
    log_to(command, log)?;
    ManagedChild::spawn(command)
}

/// One field of `/proc/PID/stat`, numbered as in proc(5). The parenthesised
/// command name is skipped first, so spaces inside it cannot shift the fields.
pub fn proc_stat_field(pid: u32, field: usize) -> io::Result<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    stat.rsplit_once(") ")
        .and_then(|(_, rest)| rest.split_whitespace().nth(field.checked_sub(3)?))
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other(format!("/proc/{pid}/stat lacks field {field}")))
}

pub struct ManagedChild {
    child: Child,
    status: Option<ExitStatus>,
}

impl ManagedChild {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        check_interrupt()?;
        let child = command.process_group(0).stdin(Stdio::null()).spawn()?;
        Ok(Self {
            child,
            status: None,
        })
    }

    pub fn signal(&self, signal: i32) -> io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        // SAFETY: the child is the leader of a group we created. It remains
        // unreaped until the group is killed, preventing reuse of its group ID.
        if unsafe { libc::kill(-(self.child.id() as libc::pid_t), signal) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn poll(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.status.is_some() {
            return Ok(self.status);
        }
        // SAFETY: waitid writes initialized siginfo storage and WNOWAIT leaves
        // the child waitable. Only this owner waits for this specific child.
        let exited = unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            if libc::waitid(
                libc::P_PID,
                self.child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            ) != 0
            {
                return Err(io::Error::last_os_error());
            }
            info.si_pid() != 0
        };
        if exited {
            // A launcher can exit before its descendants. Stop the entire group
            // before reaping the leader, even on an otherwise successful exit.
            self.signal(libc::SIGKILL)?;
            self.status = Some(self.child.wait()?);
        }
        Ok(self.status)
    }

    pub fn wait(&mut self, timeout: Duration) -> io::Result<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            check_interrupt()?;
            if let Some(status) = self.poll()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "child process deadline exceeded",
                ));
            }
            thread::sleep(POLL);
        }
    }

    pub fn stop(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.poll()? {
            return Ok(status);
        }
        self.signal(libc::SIGTERM)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.poll()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(POLL);
        }
        self.signal(libc::SIGKILL)?;
        let status = self.child.wait()?;
        self.status = Some(status);
        Ok(status)
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if self.stop().is_err() {
            let _ = self.signal(libc::SIGKILL);
            let _ = self.child.wait();
        }
    }
}

#[derive(Serialize)]
pub struct Capture {
    argv: Vec<String>,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    returncode: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandResult {
    pub argv: Vec<String>,
    pub started_at_utc: String,
    pub ended_at_utc: String,
    pub deadline_seconds: u64,
    pub elapsed_seconds: f64,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

impl CommandResult {
    /// The command spawned, met its deadline and exited zero.
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0) && self.error.is_none()
    }
}

/// Keep stdout/stderr even when spawning, waiting or the command itself fails.
pub fn run_logged(
    command: &mut Command,
    directory: &std::path::Path,
    timeout: Duration,
) -> io::Result<CommandResult> {
    std::fs::create_dir(directory)?;
    let mut result = CommandResult {
        argv: std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
        started_at_utc: crate::host::utc_now()?,
        ended_at_utc: String::new(),
        deadline_seconds: timeout.as_secs(),
        elapsed_seconds: 0.0,
        exit_code: None,
        error: None,
    };
    command
        .stdout(std::fs::File::create(directory.join("stdout.log"))?)
        .stderr(std::fs::File::create(directory.join("stderr.log"))?);
    let started = Instant::now();
    match ManagedChild::spawn(command).and_then(|mut child| child.wait(timeout)) {
        Ok(status) => result.exit_code = Some(exit_code(status)),
        Err(error) => result.error = Some(error.to_string()),
    }
    result.elapsed_seconds = started.elapsed().as_secs_f64();
    result.ended_at_utc = crate::host::utc_now()?;
    crate::evidence::write_json(&directory.join("command.json"), &result)?;
    Ok(result)
}

/// Temporary files avoid pipe backpressure and descendants holding a pipe open.
pub fn capture(argv: &[&str], cwd: &std::path::Path) -> Capture {
    let mut report = Capture {
        argv: argv.iter().map(|s| (*s).into()).collect(),
        status: "error",
        returncode: None,
        stdout: None,
        stderr: None,
        error: None,
    };
    let result = (|| -> io::Result<()> {
        let mut stdout = tempfile::tempfile()?;
        let mut stderr = tempfile::tempfile()?;
        let mut command = Command::new(argv[0]);
        command
            .args(&argv[1..])
            .current_dir(cwd)
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?);
        let mut child = ManagedChild::spawn(&mut command)?;
        let status = child.wait(Duration::from_secs(5))?;
        let read = |file: &mut std::fs::File| -> io::Result<String> {
            file.rewind()?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            Ok(String::from_utf8_lossy(&bytes).trim().into())
        };
        report.returncode = Some(exit_code(status));
        report.stdout = Some(read(&mut stdout)?);
        report.stderr = Some(read(&mut stderr)?);
        report.status = if status.success() { "ok" } else { "error" };
        Ok(())
    })();
    if let Err(error) = result {
        report.status = match error.kind() {
            io::ErrorKind::NotFound => "missing",
            io::ErrorKind::TimedOut => "timeout",
            _ => "error",
        };
        report.error = Some(error.to_string());
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn assert_stopped(pid: i32) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            // An orphan may briefly remain a zombie until init reaps it.
            match fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => return,
                Ok(stat) if stat.rsplit_once(") ").unwrap().1.starts_with('Z') => return,
                _ => {}
            }
            assert!(Instant::now() < deadline, "child {pid} is still running");
            thread::sleep(POLL);
        }
    }

    #[test]
    fn descendants_stop_on_drop_and_when_the_launcher_exits() {
        for ending in ["wait", "kill -STOP $$; wait", "exit 0"] {
            let dir = tempfile::tempdir().unwrap();
            let pid_file = dir.path().join("pid");
            let mut command = Command::new("sh");
            command
                .args([
                    "-c",
                    &format!("sleep 30 & echo $! > \"$1\"; {ending}"),
                    "test",
                ])
                .arg(&pid_file);
            let mut child = ManagedChild::spawn(&mut command).unwrap();
            // The pid and command name precede ") " and are not addressable.
            assert!(proc_stat_field(child.pid(), 2).is_err());
            let state = proc_stat_field(child.pid(), 3).unwrap();
            assert!(["R", "S", "D", "T"].contains(&state.as_str()), "{state}");
            assert!(
                proc_stat_field(child.pid(), 22)
                    .unwrap()
                    .parse::<u64>()
                    .unwrap()
                    > 0
            );
            let deadline = Instant::now() + Duration::from_secs(2);
            let pid: i32 = loop {
                if let Ok(text) = fs::read_to_string(&pid_file)
                    && let Ok(pid) = text.trim().parse()
                {
                    break pid;
                }
                assert!(
                    Instant::now() < deadline,
                    "launcher never recorded its child"
                );
                thread::sleep(POLL);
            };
            if ending == "exit 0" {
                assert!(child.wait(Duration::from_secs(2)).unwrap().success());
            } else {
                assert_eq!(
                    child.wait(Duration::from_millis(40)).unwrap_err().kind(),
                    io::ErrorKind::TimedOut
                );
            }
            drop(child);
            assert_stopped(pid);
        }
    }

    #[test]
    fn capture_preserves_failures_and_output_larger_than_a_pipe() {
        let dir = tempfile::tempdir().unwrap();
        let failed = capture(&["sh", "-c", "printf failed >&2; exit 7"], dir.path());
        assert_eq!(failed.returncode, Some(7));
        assert_eq!(failed.status, "error");
        assert_eq!(failed.stderr.as_deref(), Some("failed"));
        let large = capture(&["sh", "-c", "head -c 262144 /dev/zero"], dir.path());
        assert_eq!(large.status, "ok");
        assert_eq!(large.stdout.unwrap().len(), 262144);
        let missing = capture(&["/nonexistent/cas-harness-test"], dir.path());
        assert_eq!(missing.status, "missing");
        assert!(missing.error.is_some());
    }
}
