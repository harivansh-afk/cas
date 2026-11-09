"""Owned KVM host with ZFS support when the provider kernel lacks its module."""

import argparse
import os
import signal
import subprocess
import sys
from pathlib import Path

from agent import stop, write_json

parser = argparse.ArgumentParser(__doc__)
parser.add_argument("--vm", required=True)
parser.add_argument("--output", required=True)
parser.add_argument("--authorized-key", required=True)
parser.add_argument("--bind", required=True)
args = parser.parse_args()
signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
output = Path(args.output).absolute()
output.mkdir(parents=True, exist_ok=False)
(output / "authorized_keys").write_bytes(Path(args.authorized_key).read_bytes())
scratch = output / "scratch"
scratch.mkdir(mode=0o700)
for name in ("xfs", "zfs"):
    with (scratch / (name + ".raw")).open("xb") as disk:
        disk.truncate(8 * 1024**3)
write_json(
    output / "launcher.json", {"host": os.uname(), "vm": args.vm, "bind": args.bind}
)
with (output / "console.log").open("x") as log:
    process = subprocess.Popen(
        [args.vm],
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=log,
        start_new_session=True,
        env={
            **os.environ,
            "CAS_RESULTS_DIR": str(output),
            "CAS_BIND_ADDRESS": args.bind,
            "CAS_XFS_DISK": str(scratch / "xfs.raw"),
            "CAS_ZFS_DISK": str(scratch / "zfs.raw"),
            "TMPDIR": str(scratch),
        },
    )
    try:
        if process.wait():
            raise RuntimeError("appliance failed; inspect retained console and disks")
    finally:
        stop(process)
