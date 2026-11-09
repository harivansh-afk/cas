"""Fail-closed measurement rules; run with uv run --no-project python -m unittest."""

import json
import socket
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from agent import CLONE, IMAGE, Agent, fio_options, ssh_ready
from runner import classify, deltas, has_overlap, phases, rsync_bytes


def report(sent=0, memory=0, storage=0):
    return {
        "fleet": {
            key: {"sent": sent, "received": sent * 2}
            for key in (
                "ship",
                "sync",
                "provision",
                "migration",
                "collection",
                "control",
            )
        },
        "peer": {
            "counters": {
                "gets": 0,
                "verified": 0,
                "errors": 0,
                "server_memory": memory,
                "server_store": storage,
            }
        },
    }


class MeasurementTests(unittest.TestCase):
    def test_fio_json_is_separate_from_ssh_diagnostics(self):
        with tempfile.TemporaryDirectory() as directory:
            agent = Agent({})
            try:
                agent.case = Path(directory)
                agent.settings = {"seconds": 1}
                result = {
                    "jobs": [
                        {
                            "error": 0,
                            "read": {"io_bytes": 4096},
                            "write": {"io_bytes": 0},
                        }
                    ]
                }
                script = (
                    "import sys; print('SSH warning', file=sys.stderr); print("
                    + repr(json.dumps(result))
                    + ")"
                )
                agent.spawn("fio", [sys.executable, "-c", script], separate_stderr=True)
                self.assertEqual(agent.finish_fio("fio"), result)
                self.assertIn(
                    "SSH warning", (agent.case / "fio.stderr.log").read_text()
                )
            finally:
                agent.close()

    def test_storage_only_member_can_settle_without_attached_guests(self):
        with tempfile.TemporaryDirectory() as directory:
            agent = Agent({})
            try:
                agent.case = Path(directory)
                (agent.case / "daemon").mkdir()
                value = {"images": []}
                (agent.case / "daemon/telemetry.jsonl").write_text(
                    json.dumps(value) + "\n"
                )
                with patch("agent.time.sleep"):
                    self.assertEqual(agent.settle(), value)
            finally:
                agent.close()

    def test_wire_counts_both_senders_once_and_rejects_restart(self):
        before, after = [report(10), report(20)], [report(17), report(29)]
        self.assertEqual(deltas(before, after)["ship"], 16)
        with self.assertRaises(ValueError):
            deltas(after, before)

    def test_cache_labels_need_positive_counter_evidence(self):
        classify(report(), report(memory=5), "memory")
        classify(report(), report(storage=5), "storage")
        for target, after in (
            ("memory", report()),
            ("memory", report(memory=4, storage=1)),
            ("storage", report(memory=1, storage=4)),
        ):
            with self.assertRaises(ValueError):
                classify(report(), after, target)

    def test_rsync_requires_real_both_direction_stats(self):
        self.assertEqual(
            rsync_bytes("Total bytes sent: 1,024\nTotal bytes received: 32\n"), 1056
        )
        with self.assertRaises(ValueError):
            rsync_bytes("sent 1024 bytes")

    def test_ring_filters_prior_samples_and_rejects_failed_or_unordered_reads(self):
        record = {
            "id": 3,
            "chunks": 1,
            "received_ns": 10,
            "success": True,
            "phases": {"send_ns": 20, "first_byte_ns": 30, "verified_ns": 35},
            "completed_ns": 40,
        }
        report = {"peer": {"recent": [record]}}
        self.assertEqual(phases(report, 0)[0]["ns"], [10, 10, 5, 5])
        self.assertEqual(phases(report, 11), [])
        record["phases"]["send_ns"] = 50
        with self.assertRaises(ValueError):
            phases(report, 0)
        record["success"] = False
        with self.assertRaises(ValueError):
            phases(report, 0)

    def test_full_image_baseline_uses_tcp_and_verifies_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            a, b = Agent({}), Agent({})
            try:
                for index, agent in enumerate((a, b)):
                    agent.case = Path(directory) / str(index)
                    agent.case.mkdir()
                    agent.work = agent.case
                payload = bytes(range(256)) * 4096
                (a.work / (IMAGE + ".raw")).write_bytes(payload)
                result = a.rsync({"url": b.raw_receiver(), "work": str(b.work)}, CLONE)
                self.assertGreater(result["application_bytes"], len(payload))
                self.assertEqual((b.work / (CLONE + ".raw")).read_bytes(), payload)
                self.assertTrue(
                    any(arg.startswith("rsync://127.0.0.1:") for arg in result["argv"])
                )
            finally:
                a.close()
                b.close()

    def test_guest_readiness_requires_an_ssh_banner(self):
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen()
            port = listener.getsockname()[1]

            def banners():
                for banner in (b"HTTP/1.0 200 OK\r\n", b"SSH-2.0-test\r\n"):
                    with listener.accept()[0] as stream:
                        stream.sendall(banner)

            worker = threading.Thread(target=banners)
            worker.start()
            try:
                self.assertFalse(ssh_ready(port))
                self.assertTrue(ssh_ready(port))
            finally:
                worker.join(timeout=5)
            self.assertFalse(worker.is_alive())
        self.assertFalse(ssh_ready(port))

    def test_inventory_bytes_alone_do_not_certify_put_contention(self):
        sample = {"ship_bytes": 65536, "verified_gets": 20, "ship_chunks_acked": 0}
        self.assertFalse(has_overlap([sample]))
        sample["ship_chunks_acked"] = 1
        self.assertTrue(has_overlap([sample]))
        sample["verified_gets"] = 0
        self.assertFalse(has_overlap([sample]))

    def test_fio_keeps_verification_and_direct_io_in_read_cohorts(self):
        for kind in ("random", "sequential", "verify"):
            options = fio_options(kind, 1048576, 30, 4)
            for option in (
                "--direct=1",
                "--bs=4096",
                "--verify=crc32c",
                "--verify_fatal=1",
                "--iodepth=4",
            ):
                self.assertIn(option, options)
            self.assertNotIn("--verify=0", options)


if __name__ == "__main__":
    unittest.main()
