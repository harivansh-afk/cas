#!/usr/bin/env python3
"""Assemble node-inventory.json from the raw records setup.sh captured.
Every record file is embedded (text, or parsed JSON where it parses) so the
inventory is self-contained; a few derived fields sit at the top for the
campaign driver and for humans."""

import argparse
import hashlib
import json
import re
import socket
import time
from pathlib import Path


def load_records(records):
    hardware, errors = {}, {}
    for path in sorted((records / "hw").glob("*")):
        if path.suffix == ".err":
            errors[path.stem] = path.read_text().strip()
            continue
        text = path.read_text()
        try:
            hardware[path.stem] = json.loads(text)
        except ValueError:
            hardware[path.stem] = text.rstrip("\n")
    return hardware, errors


def load_json(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return None


def derive(hardware):
    """Fields the driver reads: logical cpu count, kernel, NIC links, governors."""
    nproc = hardware.get("nproc", "")
    try:
        logical = int(str(nproc).strip())
    except ValueError:
        logical = None
    kernel = str(hardware.get("uname", "")).split()[2] if hardware.get("uname") else None
    nics = {}
    for key, text in hardware.items():
        match = re.fullmatch(r"ethtool-(?!(?:i|g|c|k|l)-)(.+)", key)
        if not match or not isinstance(text, str):
            continue
        name = match[1]
        speed = re.search(r"Speed: (\S+)", text)
        link = re.search(r"Link detected: (\S+)", text)
        driver = hardware.get(f"ethtool-i-{name}", "")
        nics[name] = {
            "speed": speed[1] if speed else None,
            "link": link[1] if link else None,
            "mtu": str(hardware.get(f"mtu-{name}", "")).strip() or None,
            "driver": (re.search(r"driver: (\S+)", driver) or [None, None])[1] if isinstance(driver, str) else None,
            "firmware": (re.search(r"firmware-version: (.*)", driver) or [None, None])[1] if isinstance(driver, str) else None,
            "numa_node": str(hardware.get(f"numa-{name}", "")).strip() or None,
            "msi_irqs": str(hardware.get(f"msi-irqs-{name}", "")).split(),
        }
    governors = sorted({line.split()[1] for line in str(hardware.get("cpufreq", "")).splitlines() if len(line.split()) > 1})
    return {
        "logical_cpus": logical,
        "kernel": kernel,
        "nics": nics,
        "governors": governors,
        "smt": str(hardware.get("smt", "")).strip() or None,
        "cpuidle": str(hardware.get("cpuidle", "")),
        "nofile": str(hardware.get("ulimit", "")).split(),
        "nvme": [k.replace("nvme-id-ctrl-", "") for k in hardware if k.startswith("nvme-id-ctrl-")],
        "nix": str(hardware.get("nix-version", "")).strip() or None,
        "fio": str(hardware.get("fio-version", "")).strip() or None,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--records", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    records = Path(args.records)
    hardware, errors = load_records(records)
    steps = []
    steps_path = records / "steps.jsonl"
    if steps_path.exists():
        steps = [json.loads(line) for line in steps_path.read_text().splitlines() if line.strip()]
    inventory = {
        "schema": "cas-node-inventory-1",
        "hostname": socket.gethostname(),
        "recorded": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "args": load_json(records / "args.json"),
        "hardware": {**derive(hardware), "records": hardware, "unavailable": errors},
        "disk": load_json(records / "disk.json"),
        "disk_mount": load_json(records / "disk-mount.json"),
        "source": load_json(records / "source.json"),
        "build": load_json(records / "build.json"),
        "ulimit": load_json(records / "ulimit.json"),
        "irq": load_json(records / "irq.json"),
        "smt": load_json(records / "smt.json"),
        "steps": steps,
        "setup_log_sha256": hashlib.sha256((records / "setup.log").read_bytes()).hexdigest() if (records / "setup.log").exists() else None,
    }
    inventory["skipped"] = [name for name, value in (("disk", inventory["disk"]), ("build", inventory["build"]), ("irq", inventory["irq"]), ("smt", inventory["smt"])) if value is None]
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(inventory, indent=1, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
