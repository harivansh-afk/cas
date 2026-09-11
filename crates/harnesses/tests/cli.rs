//! Exercise cancellation in a separate process, including its signal handlers.
use std::fs;
use std::os::unix::fs::symlink;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

struct Harness(Child);

#[test]
fn wrong_build_is_rejected_before_any_guest_is_started() {
    let directory = tempfile::tempdir().unwrap();
    let build = directory.path().join("build.json");
    fs::write(
        &build,
        serde_json::to_vec(&serde_json::json!({
            "source_path":"/wrong-source", "harness":env!("CARGO_BIN_EXE_cas-harness"),
            "system":"aarch64-linux", "backend":"raw", "daemon":null,
        }))
        .unwrap(),
    )
    .unwrap();
    let output = directory.path().join("run");
    let result = Command::new(env!("CARGO_BIN_EXE_cas-harness"))
        .args([
            "vm",
            "--vm",
            "/nonexistent/guest",
            "--lock",
            "/nonexistent/lock",
        ])
        .arg("--build-info")
        .arg(build)
        .arg("--expect-source")
        .arg("/expected-source")
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();
    assert!(!result.status.success());
    let summary: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["passed"], false);
    assert!(
        summary["error"]
            .as_str()
            .unwrap()
            .contains("expected build")
    );
    assert!(!output.join("console.log").exists());
}

impl Drop for Harness {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            // SAFETY: this is our unreaped child, so its PID cannot be reused.
            unsafe {
                libc::kill(self.0.id() as i32, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(7);
            while self.0.try_wait().unwrap().is_none() {
                if Instant::now() >= deadline {
                    self.0.kill().unwrap();
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            self.0.wait().unwrap();
        }
    }
}

#[test]
fn interruption_stops_the_active_inventory_probe() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = std::env::split_paths(&std::env::var_os("PATH").unwrap()).collect();
        let git = dir.path().join("git");
        symlink(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/launcher.sh"),
            &git,
        )
        .unwrap();
        fs::write(
            dir.path().join("fixture.sh"),
            "printf '%s\\n' \"$$\" > \"$CAS_TEST_PID\"\nexec sleep 30\n",
        )
        .unwrap();
        let pid_file = dir.path().join("probe.pid");
        let path =
            std::env::join_paths(std::iter::once(dir.path().to_path_buf()).chain(paths)).unwrap();
        let mut harness = Harness(
            Command::new(env!("CARGO_BIN_EXE_cas-harness"))
                .args(["preflight", "--label", "cancellation", "--output"])
                .arg(dir.path().join("inventory.json"))
                .arg("--checkout")
                .arg(dir.path())
                .env("PATH", path)
                .env("CAS_TEST_PID", &pid_file)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        let probe: i32 = loop {
            if let Ok(text) = fs::read_to_string(&pid_file)
                && let Ok(pid) = text.trim().parse()
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "inventory probe never started");
            thread::sleep(Duration::from_millis(20));
        };
        // SAFETY: the harness remains owned and unreaped throughout this test.
        assert_eq!(unsafe { libc::kill(harness.0.id() as i32, signal) }, 0);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = harness.0.try_wait().unwrap() {
                assert!(!status.success());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "harness did not respond to cancellation"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !std::path::Path::new(&format!("/proc/{probe}")).exists(),
            "probe survived cancellation"
        );
    }
}
