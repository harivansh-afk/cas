import importlib.util
import os
from pathlib import Path
import stat
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "check_disks", Path(__file__).resolve().parents[1] / "check-disks.py"
)
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)


class DiskTests(unittest.TestCase):
    def test_aliases_of_the_same_device_are_rejected(self):
        device = SimpleNamespace(st_mode=stat.S_IFBLK, st_rdev=os.makedev(8, 0))
        with patch.object(Path, "stat", return_value=device), patch.object(
            Path, "exists", side_effect=[True, False, True, False]
        ):
            with self.assertRaisesRegex(ValueError, "same block device"):
                check.check_disks(
                    Path("/dev/disk/by-id/ata-disk"),
                    Path("/dev/disk/by-id/wwn-disk"),
                )

    def test_distinct_whole_disks_are_accepted(self):
        devices = [
            SimpleNamespace(st_mode=stat.S_IFBLK, st_rdev=os.makedev(8, minor))
            for minor in (0, 16)
        ]
        with patch.object(Path, "stat", side_effect=devices), patch.object(
            Path, "exists", side_effect=[True, False, True, False]
        ):
            check.check_disks(Path("/dev/disk/by-id/os"), Path("/dev/disk/by-id/data"))

    def test_regular_file_is_rejected(self):
        with patch.object(
            Path, "stat", return_value=SimpleNamespace(st_mode=stat.S_IFREG)
        ):
            with self.assertRaisesRegex(ValueError, "not a block device"):
                check.disk_identity(Path("/dev/disk/by-id/file"))

    def test_partition_is_rejected(self):
        device = SimpleNamespace(st_mode=stat.S_IFBLK, st_rdev=os.makedev(8, 1))
        with patch.object(Path, "stat", return_value=device), patch.object(
            Path, "exists", return_value=True
        ):
            with self.assertRaisesRegex(ValueError, "not a whole disk"):
                check.disk_identity(Path("/dev/disk/by-id/disk-part1"))

    def test_unstable_path_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "stable"):
            check.disk_identity(Path("/dev/sda"))


if __name__ == "__main__":
    unittest.main()
