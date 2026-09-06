//! Child process groups with deadlines and cleanup on every return path.
use std::io::{self, Read, Seek};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

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

pub struct ManagedChild {
    child: Child,
    status: Option<ExitStatus>,
}

impl ManagedChild {
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
        for ending in ["wait", "exit 0"] {
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
