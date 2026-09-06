"""Run the pinned KVM guest on a new scratch image and retain its evidence."""

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import subprocess
import sys
import tempfile
import time


IO_BYTES = 64 * 1024 * 1024
DISK_BYTES = 128 * 1024 * 1024
# Linux 6.17.13 can leave io-wq workers asleep for their five-second idle timeout.
# See docs/review/daemon-lifetime.md. Keep process shutdown bounded.
DAEMON_SHUTDOWN_SECONDS = 10


def read_json(path: Path) -> dict:
    value = json.loads(path.read_text())
    if not isinstance(value, dict):
        raise ValueError(f"expected a JSON object in {path.name}")
    return value


def verify_guest(completion: dict, fio: dict, job_name: str = "raw-smoke", *, read_only: bool = False) -> None:
    if (
        completion.get("schema_version") != 1
        or completion.get("service_result") != "success"
        or completion.get("exit_code") != "exited"
        or completion.get("exit_status") != "0"
    ):
        raise ValueError(f"guest service did not succeed: {completion}")
    verify_fio(fio, job_name, write_bytes=0 if read_only else IO_BYTES)


def verify_fio(fio: dict, job_name: str, *, write_bytes: int = IO_BYTES, read_bytes: int = IO_BYTES) -> None:
    jobs = fio.get("jobs")
    if not isinstance(jobs, list) or len(jobs) != 1 or not isinstance(jobs[0], dict):
        raise ValueError("fio did not report exactly one job")
    job = jobs[0]
    if job.get("jobname") != job_name or job.get("error") != 0:
        raise ValueError("fio job failed or has an unexpected name")
    for direction, expected in (("write", write_bytes), ("read", read_bytes)):
        stats = job.get(direction)
        if not isinstance(stats, dict) or stats.get("io_bytes") != expected:
            raise ValueError(f"fio did not complete the expected {direction} byte count")


def verify_daemon(report: dict, backend: str = "raw_io_uring", *, read_only: bool = False) -> None:
    if (report.get("schema_version") != 1 or report.get("backend") != backend
        or report.get("connection_ok") is not True or report.get("errors") != 0
        or report.get("flush_negotiated") is not True
        or report.get("pending_at_disconnect") != 0 or report.get("queues") != 1):
        raise ValueError("daemon did not report a clean run")
    for name, minimum in (("write_bytes", 0 if read_only else 2 * IO_BYTES),
                          ("read_bytes", IO_BYTES if read_only else 2 * IO_BYTES),
                          ("flushes", 0 if read_only else 2), ("bounce_requests", 1),
                          ("peak_inflight", 1 if read_only else 2)):
        value = report.get(name)
        if type(value) is not int or value < minimum:
            raise ValueError(f"daemon did not complete expected {name}")
    if read_only and report["write_bytes"] != 0:
        raise ValueError("recovery verification wrote to the image")
    if backend == "staging_sync":
        staging = report.get("staging")
        expected = (IO_BYTES if read_only else 2 * IO_BYTES) // 4096
        if (not isinstance(staging, dict) or staging.get("durable") != expected
            or staging.get("appended") != expected or staging.get("image_bytes") != DISK_BYTES):
            raise ValueError("staging did not confirm the expected durable prefix")


def prepare_output(path: Path) -> Path:
    if path.is_symlink():
        raise ValueError("output must be a new directory, not a symlink")
    path = path.resolve()
    # QEMU parses commas inside -drive/-virtfs even when the shell quotes paths.
    if any(character in str(path) for character in (",", "\n", "\r")):
        raise ValueError("output path cannot contain commas or line breaks")
    path.mkdir(parents=True)  # No exist_ok: old results must never make a failed run pass.
    return path


def capture(argv: list[str]) -> dict:
    try:
        result = subprocess.run(argv, capture_output=True, text=True, timeout=5, check=False)
        return {
            "argv": argv,
            "returncode": result.returncode,
            "stdout": result.stdout.strip(),
            "stderr": result.stderr.strip(),
        }
    except (OSError, subprocess.TimeoutExpired) as error:
        return {"argv": argv, "error": str(error)}


def stop_vm(process: subprocess.Popen) -> None:
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=5)
    except ProcessLookupError:
        process.wait()
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()


def execute_guest(args: argparse.Namespace, build: dict, output: Path, image: Path,
                  evidence: dict, *, phase: str | None = None) -> None:
    """One guest boot; recovery's write phase ends in a deliberate daemon SIGKILL."""
    process = daemon = daemon_log = socket_directory = None
    results = output / "guest"
    results.mkdir()
    temporary = output / "tmp"
    temporary.mkdir()
    if phase is not None:
        (results / "recovery-phase").write_text(phase + "\n")
    env = {
        key: value for key, value in os.environ.items()
        if not key.startswith(("QEMU_", "NIX_GUEST_")) and key not in ("NIX_DISK_IMAGE", "SHARED_DIR")
    }
    env.update({"CAS_RAW_IMAGE": str(image), "CAS_RESULTS_DIR": str(results),
                "TMPDIR": str(temporary), "USE_TMPDIR": "1"})
    try:
        if build["backend"] in ("daemon", "staging"):
            socket_directory = tempfile.TemporaryDirectory(prefix="cas-vhost-", dir="/tmp")
            socket = Path(socket_directory.name) / "block.sock"
            env["CAS_VHOST_SOCKET"] = str(socket)
            command = [build["daemon"], "--socket", str(socket), "--image", str(image),
                       "--report", str(output / "daemon.json")]
            if build["backend"] == "staging":
                command += ["--backend", "staging"]
                if phase != "read":
                    command += ["--create-bytes", str(DISK_BYTES)]
            daemon_log = (output / "daemon.log").open("wb")
            daemon = subprocess.Popen(command, cwd=output, env=env, stdin=subprocess.DEVNULL,
                                      stdout=daemon_log, stderr=subprocess.STDOUT, start_new_session=True)
            deadline = time.monotonic() + 5
            while not socket.is_socket():
                if daemon.poll() is not None or time.monotonic() >= deadline:
                    raise ValueError("daemon failed to open its socket; see daemon.log")
                time.sleep(0.02)
        command = [str(args.vm)]
        evidence["launcher"] = command
        with (output / "console.log").open("wb") as console:
            process = subprocess.Popen(command, cwd=output, env=env, stdin=subprocess.DEVNULL,
                                       stdout=console, stderr=subprocess.STDOUT, start_new_session=True)
            if phase == "write":
                deadline = time.monotonic() + args.timeout
                marker = results / "write-flushed.json"
                while not marker.exists():
                    if process.poll() is not None or daemon.poll() is not None:
                        raise ValueError("guest or daemon exited before the recovery FLUSH marker")
                    if time.monotonic() >= deadline:
                        raise ValueError("guest did not confirm recovery FLUSH before the deadline")
                    time.sleep(0.02)
                if read_json(marker) != {"schema_version": 1, "phase": "write_flushed"}:
                    raise ValueError("invalid recovery FLUSH marker")
                verify_fio(read_json(results / "recovery.json"), "recovery-write", read_bytes=0)
                # Guest is waiting after fio end_fsync and blockdev --flushbufs.
                # Kill the backend before stopping QEMU; do not grant a shutdown flush.
                os.killpg(daemon.pid, signal.SIGKILL)
                evidence["daemon_exit"] = daemon.wait(timeout=DAEMON_SHUTDOWN_SECONDS)
                if evidence["daemon_exit"] != -signal.SIGKILL:
                    raise ValueError("recovery did not kill the live daemon")
                evidence["flush_marker"] = read_json(marker)
                evidence["written_bytes"] = IO_BYTES
                stop_vm(process)
                evidence["guest_exit"] = process.returncode
                return
            evidence["guest_exit"] = process.wait(timeout=args.timeout)
        if evidence["guest_exit"] != 0:
            raise ValueError(f"QEMU exited with {evidence['guest_exit']}; see console.log")
        completion = read_json(results / "completion.json")
        if phase == "read":
            verify_guest(completion, read_json(results / "recovery.json"), "recovery-read", read_only=True)
            evidence["verified_bytes"] = IO_BYTES
        else:
            verify_guest(completion, read_json(results / "fio.json"))
            evidence["verified_bytes"] = IO_BYTES
            if daemon is not None:
                verify_guest(completion, read_json(results / "queue.json"), "queue-smoke")
                evidence["verified_bytes"] = 2 * IO_BYTES
        if daemon is not None:
            shutdown_started = time.monotonic()
            try:
                evidence["daemon_exit"] = daemon.wait(timeout=DAEMON_SHUTDOWN_SECONDS)
            finally:
                evidence["daemon_shutdown_seconds"] = time.monotonic() - shutdown_started
            if evidence["daemon_exit"] != 0:
                raise ValueError("daemon failed; see daemon.log")
            evidence["daemon"] = read_json(output / "daemon.json")
            verify_daemon(evidence["daemon"], "staging_sync" if build["backend"] == "staging" else "raw_io_uring",
                          read_only=phase == "read")
    finally:
        if process is not None:
            stop_vm(process)
        if daemon is not None:
            stop_vm(daemon)
        if daemon_log is not None:
            daemon_log.close()
        if socket_directory is not None:
            socket_directory.cleanup()


def run(args: argparse.Namespace) -> int:
    output = prepare_output(args.output)
    summary = {
        "schema_version": 1, "artifact": "development_raw_vm_smoke", "passed": False,
        "paper_gate": None, "started_at_utc": datetime.now(timezone.utc).isoformat(),
        "host_machine": platform.machine(), "host_kernel": platform.release(),
        "host_cpu_affinity": sorted(os.sched_getaffinity(0)), "disk_bytes": DISK_BYTES,
        "verified_bytes": 0, "guest_exit": None,
    }
    started = time.monotonic()
    try:
        with open("/dev/kvm", "rb+"):
            pass
        build = read_json(args.build_info)
        summary["build"] = build
        backend = build["backend"]
        if backend not in ("raw", "daemon", "staging"):
            raise ValueError("unknown build backend")
        if args.recovery and backend != "staging":
            raise ValueError("--recovery requires the staging runner")
        summary["artifact"] = f"development_{backend}_vm_{'recovery' if args.recovery else 'smoke'}"
        if build["system"] != f"{platform.machine()}-linux":
            raise ValueError("guest architecture must match the KVM host")
        shutil.copyfile(args.lock, output / "flake.lock")
        shutil.copyfile(args.build_info, output / "build.json")
        summary["invocation_worktree"] = capture(["git", "status", "--porcelain"])
        disk_dir = args.disk_dir.resolve(strict=True) if args.disk_dir else output
        if not disk_dir.is_dir() or any(c in str(disk_dir) for c in (",", "\n", "\r")):
            raise ValueError("disk directory must exist and contain no commas or line breaks")
        # Preserve the scratch image with the evidence. Never accept an existing device.
        if backend == "staging":
            image = Path(tempfile.mkdtemp(prefix="cas-staging-", dir=disk_dir)) / "image.log"
        else:
            descriptor, image_name = tempfile.mkstemp(prefix="cas-smoke-", suffix=".raw", dir=disk_dir)
            image = Path(image_name)
            with os.fdopen(descriptor, "wb") as disk:
                os.posix_fallocate(disk.fileno(), 0, DISK_BYTES)
                os.fsync(disk.fileno())
            summary["raw_image"] = str(image)
        summary["storage_image"] = str(image)
        summary["host_filesystem"] = capture([
            "findmnt", "--json", "--target", str(image.parent), "--output", "TARGET,SOURCE,FSTYPE,OPTIONS"
        ])
        if args.recovery:
            summary["phases"] = {}
            for phase in ("write", "read"):
                phase_output = output / phase
                phase_output.mkdir()
                evidence = summary["phases"][phase] = {}
                execute_guest(args, build, phase_output, image, evidence, phase=phase)
            summary["verified_bytes"] = summary["phases"]["read"]["verified_bytes"]
        else:
            execute_guest(args, build, output, image, summary)
        summary["passed"] = True
    except (OSError, ValueError, subprocess.TimeoutExpired) as error:
        summary["error"] = str(error)
    finally:
        summary["wall_seconds_including_boot"] = time.monotonic() - started
        (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps({"passed": summary["passed"], "results": str(output), "paper_gate": None}))
    if not summary["passed"]:
        print(summary.get("error", "guest check failed"), file=sys.stderr)
    return 0 if summary["passed"] else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="New result directory")
    parser.add_argument("--disk-dir", type=Path, help="Existing filesystem directory for the new scratch disk; defaults to output")
    parser.add_argument("--recovery", action="store_true", help="Kill staging after guest FLUSH, then verify from a fresh guest")
    parser.add_argument("--timeout", type=int, default=90, help="Guest timeout in seconds (1–100)")
    parser.add_argument("--vm", type=Path, required=True, help=argparse.SUPPRESS)
    parser.add_argument("--build-info", type=Path, required=True, help=argparse.SUPPRESS)
    parser.add_argument("--lock", type=Path, required=True, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not 1 <= args.timeout <= 100:
        parser.error("timeout must be between 1 and 100 seconds")
    try:
        return run(args)
    except (OSError, ValueError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    sys.exit(main())
