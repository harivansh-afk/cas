import copy
import importlib.util
from pathlib import Path
import tempfile
import unittest


spec = importlib.util.spec_from_file_location("run_vm", Path(__file__).parents[1] / "run-vm.py")
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class GuestEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.completion = {
            "schema_version": 1, "service_result": "success", "exit_code": "exited", "exit_status": "0"
        }
        self.fio = {"jobs": [{
            "jobname": "raw-smoke", "error": 0,
            "read": {"io_bytes": runner.IO_BYTES}, "write": {"io_bytes": runner.IO_BYTES},
        }]}

    def test_complete_verified_run(self):
        runner.verify_guest(self.completion, self.fio)

    def test_failed_or_incomplete_service_cannot_pass(self):
        for field, value in [
            ("service_result", "timeout"), ("exit_code", "killed"), ("exit_status", "1"),
            ("schema_version", 0),
        ]:
            with self.subTest(field=field):
                completion = {**self.completion, field: value}
                with self.assertRaises(ValueError):
                    runner.verify_guest(completion, self.fio)

    def test_fio_verify_error_cannot_pass(self):
        self.fio["jobs"][0]["error"] = 84
        with self.assertRaises(ValueError):
            runner.verify_guest(self.completion, self.fio)

    def test_partial_io_cannot_pass(self):
        for direction in ("write", "read"):
            with self.subTest(direction=direction):
                fio = copy.deepcopy(self.fio)
                fio["jobs"][0][direction]["io_bytes"] -= 4096
                with self.assertRaises(ValueError):
                    runner.verify_guest(self.completion, fio)

    def test_missing_or_wrong_job_cannot_pass(self):
        for jobs in [[], None, [None], [{"jobname": "unrelated", "error": 0}]]:
            with self.subTest(jobs=jobs):
                with self.assertRaises(ValueError):
                    runner.verify_guest(self.completion, {"jobs": jobs})

    def test_output_refuses_stale_results(self):
        with tempfile.TemporaryDirectory() as parent:
            output = runner.prepare_output(Path(parent) / "results with spaces")
            marker = output / "summary.json"
            marker.write_text("previous result")
            with self.assertRaises(FileExistsError):
                runner.prepare_output(output)
            self.assertEqual(marker.read_text(), "previous result")

    def test_qemu_separator_is_rejected_before_creating_directory(self):
        with tempfile.TemporaryDirectory() as parent:
            for name in ["a,b", "a\nb", "a\rb"]:
                with self.subTest(name=name):
                    output = Path(parent) / name
                    with self.assertRaises(ValueError):
                        runner.prepare_output(output)
                    self.assertFalse(output.exists())


class DaemonEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.report = {
            "schema_version": 1, "backend": "raw_io_uring", "connection_ok": True,
            "errors": 0, "pending_at_disconnect": 0, "queues": 1,
            "write_bytes": 2 * runner.IO_BYTES, "read_bytes": 2 * runner.IO_BYTES,
            "flushes": 514, "bounce_requests": 65536, "peak_inflight": 32,
        }

    def test_complete_daemon_run(self):
        runner.verify_daemon(self.report)

    def test_failed_incomplete_or_serial_run_cannot_pass(self):
        for field, value in [
            ("connection_ok", False), ("errors", 1), ("pending_at_disconnect", 1),
            ("write_bytes", runner.IO_BYTES), ("read_bytes", 0), ("flushes", 0),
            ("peak_inflight", 1), ("backend", "raw"), ("bounce_requests", None),
        ]:
            with self.subTest(field=field), self.assertRaises(ValueError):
                runner.verify_daemon({**self.report, field: value})


if __name__ == "__main__":
    unittest.main()
