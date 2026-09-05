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


def read_json(path: Path) -> dict:
    value = json.loads(path.read_text())
    if not isinstance(value, dict):
        raise ValueError(f"expected a JSON object in {path.name}")
    return value


def verify_guest(completion: dict, fio: dict) -> None:
    if (
        completion.get("schema_version") != 1
        or completion.get("service_result") != "success"
        or completion.get("exit_code") != "exited"
        or completion.get("exit_status") != "0"
    ):
        raise ValueError(f"guest service did not succeed: {completion}")
    jobs = fio.get("jobs")
    if not isinstance(jobs, list) or len(jobs) != 1 or not isinstance(jobs[0], dict):
        raise ValueError("fio did not report exactly one job")
    job = jobs[0]
    if job.get("jobname") != "raw-smoke" or job.get("error") != 0:
        raise ValueError("fio job failed or has an unexpected name")
    for direction in ("write", "read"):
        stats = job.get(direction)
        if not isinstance(stats, dict) or stats.get("io_bytes") != IO_BYTES:
            raise ValueError(f"fio did not complete the expected {direction} byte count")


def prepare_output(path: Path) -> Path:
    if path.is_symlink():
        raise ValueError("output must be a new directory, not a symlink")
    path = path.resolve()
    # QEMU parses commas inside -drive/-virtfs even when the shell quotes paths.
    if any(character in str(path) for character in (",", "\n", "\r")):
        raise ValueError("output path cannot contain commas or line breaks")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.mkdir()  # No exist_ok: old results must never make a failed run pass.
    return path.resolve()


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


def run(args: argparse.Namespace) -> int:
    output = prepare_output(args.output)
    summary = {
        "schema_version": 1,
        "artifact": "development_raw_vm_smoke",
        "passed": False,
        "paper_gate": None,
        "started_at_utc": datetime.now(timezone.utc).isoformat(),
        "host_machine": platform.machine(),
        "host_kernel": platform.release(),
        "host_cpu_affinity": sorted(os.sched_getaffinity(0)),
        "disk_bytes": DISK_BYTES,
        "verified_bytes": IO_BYTES,
        "guest_exit": None,
    }
    process = None
    started = time.monotonic()
    try:
        # Fail before boot rather than silently measuring CPU emulation.
        with open("/dev/kvm", "rb+"):
            pass
        build = read_json(args.build_info)
        summary["build"] = build
        if build["system"] != f"{platform.machine()}-linux":
            raise ValueError("guest architecture must match the KVM host")
        shutil.copyfile(args.lock, output / "flake.lock")
        shutil.copyfile(args.build_info, output / "build.json")
        summary["invocation_worktree"] = capture(["git", "status", "--porcelain"])

        disk_dir = args.disk_dir.resolve(strict=True) if args.disk_dir else output
        if not disk_dir.is_dir() or any(c in str(disk_dir) for c in (",", "\n", "\r")):
            raise ValueError("disk directory must exist and contain no commas or line breaks")
        # A fresh file on the selected filesystem; never open a supplied device.
        descriptor, image_name = tempfile.mkstemp(prefix="cas-smoke-", suffix=".raw", dir=disk_dir)
        image = Path(image_name)
        summary["raw_image"] = str(image)
        summary["host_filesystem"] = capture([
            "findmnt", "--json", "--target", str(image), "--output", "TARGET,SOURCE,FSTYPE,OPTIONS"
        ])
        with os.fdopen(descriptor, "wb") as disk:
            os.posix_fallocate(disk.fileno(), 0, DISK_BYTES)
            os.fsync(disk.fileno())
        results = output / "guest"
        results.mkdir()
        temporary = output / "tmp"
        temporary.mkdir()
        env = {
            key: value for key, value in os.environ.items()
            if not key.startswith(("QEMU_", "NIX_GUEST_")) and key not in ("NIX_DISK_IMAGE", "SHARED_DIR")
        }
        env.update({
            "CAS_RAW_IMAGE": str(image),
            "CAS_RESULTS_DIR": str(results),
            "TMPDIR": str(temporary),
            "USE_TMPDIR": "1",
        })
        command = [str(args.vm)]
        summary["launcher"] = command
        with (output / "console.log").open("wb") as console:
            process = subprocess.Popen(
                command, cwd=output, env=env, stdin=subprocess.DEVNULL,
                stdout=console, stderr=subprocess.STDOUT, start_new_session=True,
            )
            summary["guest_exit"] = process.wait(timeout=args.timeout)
        if summary["guest_exit"] != 0:
            raise ValueError(f"QEMU exited with {summary['guest_exit']}; see console.log")
        verify_guest(read_json(results / "completion.json"), read_json(results / "fio.json"))
        summary["passed"] = True
    except (OSError, ValueError, subprocess.TimeoutExpired) as error:
        summary["error"] = str(error)
    finally:
        if process is not None:
            stop_vm(process)
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
