# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Verify whole-disk identities on the target before provisioning; never writes."""

import argparse
import os
from pathlib import Path
import stat


def disk_identity(path: Path) -> int:
    if path.parent != Path("/dev/disk/by-id"):
        raise ValueError(f"use a stable /dev/disk/by-id path: {path}")
    device = path.stat()  # Follow aliases to the actual device.
    if not stat.S_ISBLK(device.st_mode):
        raise ValueError(f"not a block device: {path}")
    major, minor = os.major(device.st_rdev), os.minor(device.st_rdev)
    sysfs = Path(f"/sys/dev/block/{major}:{minor}")
    if not sysfs.exists() or (sysfs / "partition").exists():
        raise ValueError(f"not a whole disk: {path}")
    return device.st_rdev


def check_disks(os_disk: Path, data_disk: Path) -> None:
    if disk_identity(os_disk) == disk_identity(data_disk):
        raise ValueError("OS and experiment paths identify the same block device")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("os_disk", type=Path)
    parser.add_argument("data_disk", type=Path)
    args = parser.parse_args()
    try:
        check_disks(args.os_disk, args.data_disk)
    except (OSError, ValueError) as error:
        parser.exit(1, f"disk check failed: {error}\n")
    print("OS and experiment paths identify distinct whole block devices.")


if __name__ == "__main__":
    main()
