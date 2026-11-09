"""Unprivileged outer KVM launcher, with two disposable XFS disks."""

import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path

from agent import stop, write_json

parser = argparse.ArgumentParser(__doc__)
parser.add_argument("--vm", required=True)
parser.add_argument("--output", required=True)
parser.add_argument(
    "--cases", help="Comma-separated cases; defaults to the complete suite"
)
args = parser.parse_args()
signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
output = Path(args.output).absolute()
output.mkdir(parents=True, exist_ok=False)
if args.cases:
    (output / "cases").write_text(args.cases)
write_json(
    output / "launcher.json",
    {
        "host": os.uname(),
        "affinity": sorted(os.sched_getaffinity(0)),
        "loadavg": os.getloadavg(),
        "vm": args.vm,
    },
)
scratch = output / "scratch"
scratch.mkdir(mode=0o700)
disks = [str(scratch / name) for name in ("a.raw", "b.raw")]
for disk in disks:
    with open(disk, "xb") as file:
        file.truncate(8 * 1024**3)
env = {
    **os.environ,
    "CAS_RESULTS_DIR": str(output),
    "CAS_FLEET_A": disks[0],
    "CAS_FLEET_B": disks[1],
    "TMPDIR": str(scratch),
}
with (output / "console.log").open("x") as log:
    process = subprocess.Popen(
        [args.vm],
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=log,
        start_new_session=True,
    )
    sampled = threading.Event()

    def sample():
        with (output / "host-pressure.jsonl").open("x") as evidence:
            while not sampled.is_set():
                evidence.write(
                    json.dumps(
                        {
                            "unix_ns": time.time_ns(),
                            "loadavg": os.getloadavg(),
                            "pressure": {
                                name: Path("/proc/pressure", name).read_text()
                                for name in ("cpu", "io", "memory")
                            },
                        }
                    )
                    + "\n"
                )
                evidence.flush()
                sampled.wait(5)

    sampler = threading.Thread(target=sample)
    sampler.start()
    try:
        status = process.wait(timeout=14500)
        result = json.loads((output / "run/results.json").read_text())
        completion = json.loads((output / "completion.json").read_text())
        if status or not result["success"] or completion["service_result"] != "success":
            raise RuntimeError(
                "fixture failed; inspect retained results and scratch disks"
            )
    finally:
        sampled.set()
        try:
            stop(process)
        finally:
            sampler.join()
# Failures retain the disposable disks for diagnosis, as well as the logs.
shutil.rmtree(scratch)
