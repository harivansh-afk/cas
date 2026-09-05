# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Capture an environment inventory. This is not a benchmark or a gate pass."""

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import platform
import shutil
import subprocess


ROOT = Path(__file__).resolve().parents[1]


def capture(argv: list[str]) -> dict:
    if shutil.which(argv[0]) is None:
        return {"argv": argv, "status": "missing"}
    try:
        result = subprocess.run(
            argv, cwd=ROOT, capture_output=True, text=True, timeout=5, check=False
        )
    except subprocess.TimeoutExpired:
        return {"argv": argv, "status": "timeout"}
    except OSError as error:
        return {"argv": argv, "status": "error", "error": str(error)}
    return {
        "argv": argv,
        "status": "ok" if result.returncode == 0 else "error",
        "returncode": result.returncode,
        "stdout": result.stdout.strip(),
        "stderr": result.stderr.strip(),
    }


def read_setting(path: Path) -> dict:
    try:
        return {"path": str(path), "status": "ok", "value": path.read_text().strip()}
    except OSError as error:
        return {"path": str(path), "status": "unavailable", "error": str(error)}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--label", required=True, help="User-supplied host/run label")
    parser.add_argument("--output", required=True, type=Path, help="New JSON output path")
    args = parser.parse_args()
    # Refuse before collecting anything if a result already occupies the name.
    with args.output.open("x") as output:
        commands = {
            "revision": ["git", "rev-parse", "HEAD"],
            "worktree": ["git", "status", "--porcelain"],
            "kernel": ["uname", "-a"],
            "cpu": ["lscpu", "--json"],
            "block_devices": [
                "lsblk", "--json", "--bytes", "--output",
                "NAME,TYPE,SIZE,MODEL,ROTA,MOUNTPOINTS,FSTYPE,LOG-SEC,PHY-SEC",
            ],
            "checkout_filesystem": ["findmnt", "--json", "--target", str(ROOT)],
            "network_links": ["ip", "-details", "-json", "link", "show"],
            "rustc": ["rustc", "--version", "--verbose"],
            "cargo": ["cargo", "--version"],
            "fio": ["fio", "--version"],
            "qemu_x86_64": ["qemu-system-x86_64", "--version"],
            "qemu_aarch64": ["qemu-system-aarch64", "--version"],
            "qemu_img": ["qemu-img", "--version"],
            "zfs": ["zfs", "--version"],
        }
        settings = [Path("/proc/meminfo"), Path("/sys/kernel/mm/transparent_hugepage/enabled")]
        settings.extend(sorted(Path("/sys/devices/system/cpu").glob("cpu*/cpufreq/scaling_governor")))
        artifact = {
            "schema_version": 1,
            "artifact": "environment_inventory",
            "captured_at_utc": datetime.now(timezone.utc).isoformat(),
            "label": args.label,
            "machine": platform.machine(),
            "python": platform.python_version(),
            "commands": {name: capture(argv) for name, argv in commands.items()},
            "settings": [read_setting(path) for path in settings],
            "paper_gate": None,
        }
        json.dump(artifact, output, indent=2)
        output.write("\n")


if __name__ == "__main__":
    main()
