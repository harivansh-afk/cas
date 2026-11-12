"""Fail-closed measurement rules; run with uv run --no-project python -m unittest."""

import hashlib
import json
import socket
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from agent import (
    CLONE,
    IMAGE,
    Agent,
    effective_sizing,
    fio_options,
    image_id,
    parse_block_stat,
    sizing_args,
    ssh_ready,
)
from runner import (
    DEFAULT_CASES,
    aggregate_fio,
    apply_overrides,
    burst_guest_summary,
    classify,
    deltas,
    has_overlap,
    normalize_settings,
    peer_counters,
    phases,
    plan_cases,
    rsync_bytes,
)


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


def config(**settings):
    return {"nodes": [{"bulk": "a"}, {"bulk": "b"}], "settings": settings}


class HarnessSchemaTests(unittest.TestCase):
    """Config parsing, case selection and results schema for the burst work."""

    def test_defaults_keep_one_guest_and_fill_burst_and_resume(self):
        settings = normalize_settings(config())
        self.assertEqual(settings["guests_per_host"], [1])
        self.assertEqual(settings["guest"], {"cores": 2, "memory_mib": 1024})
        self.assertEqual(settings["burst"]["images"], 4)
        self.assertFalse(settings["burst"]["force_remote"])
        self.assertEqual(settings["burst"]["guest"], settings["guest"])
        self.assertEqual(settings["resume"]["setup_bytes"], settings["data_bytes"])
        self.assertTrue(settings["resume"]["settle"])
        scalar = normalize_settings(config(guests_per_host=4))
        self.assertEqual(scalar["guests_per_host"], [4])

    def test_inconsistent_multi_guest_settings_fail_loudly(self):
        for bad in (
            {"guests_per_host": [1, 1]},
            {"guests_per_host": [0]},
            {"guests_per_host": [17]},
            {"guests_per_host": [16], "cache_bytes": 2 * 8 * 1024**2 * 15},
            {"guest": {"cores": 0, "memory_mib": 512}},
            {"guest": {"cores": 1}},
            {"burst": {"force_remote": True, "k": 1}},
            {"burst": {"k": 3}},
            {"burst": {"images": 33}},
            {"burst": {"base_bytes": 4097}},
            {"burst": {"unknown": 1}},
            {"burst": {"base_bytes": 400 * 1024**2, "unique_bytes": 4096}},
            {"resume": {"setup_bytes": 0}},
            {"resume": {"k": 0}},
            {"resume": {"setup_script": ["not", "a", "string"]}},
        ):
            with self.subTest(bad=bad), self.assertRaises((ValueError, TypeError)):
                normalize_settings(config(**bad))
        remote = normalize_settings(config(burst={"force_remote": True, "k": 2}))
        self.assertTrue(remote["burst"]["force_remote"])

    def test_case_plan_expands_guest_arms_and_keeps_legacy_names(self):
        cfg = config(guests_per_host=[1, 4])
        cfg["cases"] = [
            "reads-memory",
            "reads-storage-g8",
            "burst-images",
            "resume-elsewhere",
            "raw-rsync",
            "flows-k2",
        ]
        plan = plan_cases(cfg, normalize_settings(cfg))
        self.assertEqual(
            [name for name, _, _ in plan],
            [
                "reads-memory",
                "reads-memory-g4",
                "reads-storage-g8",
                "burst-images",
                "resume-elsewhere",
                "raw-rsync",
                "flows-k2",
            ],
        )
        self.assertEqual(plan[0][1:], ("reads", ("memory", 1)))
        self.assertEqual(plan[2][1:], ("reads", ("storage", 8)))
        self.assertEqual(plan[3][1:], ("burst", None))
        self.assertEqual(plan[6][1:], ("flows", 2))
        default = plan_cases(config(), normalize_settings(config()))
        self.assertEqual([name for name, _, _ in default], DEFAULT_CASES)
        for bad in (["reads-memory-g0"], ["reads-memory-g99"], ["burst"]):
            cfg["cases"] = bad
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                plan_cases(cfg, normalize_settings(cfg))

    def test_overrides_only_touch_known_settings(self):
        cfg = apply_overrides(config(seconds=9), ["guests_per_host=[2]", "seconds=5"])
        self.assertEqual(cfg["settings"], {"seconds": 5, "guests_per_host": [2]})
        for bad in ("cases=[]", "guests_per_host", "=1"):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                apply_overrides(config(), [bad])

    def test_image_ids_are_distinct_and_keep_legacy_constants(self):
        self.assertEqual(image_id(IMAGE, 0), IMAGE)
        self.assertEqual(image_id(CLONE, 0), CLONE)
        ids = [image_id(family, i) for family in (IMAGE, CLONE) for i in range(32)]
        bootstrap = [(128 + n).to_bytes(16, "big").hex() for n in range(1, 16)]
        self.assertEqual(len(set(ids + bootstrap)), len(ids) + len(bootstrap))
        self.assertTrue(all(len(i) == 32 and int(i, 16) for i in ids))
        with self.assertRaises(ValueError):
            image_id("05" * 16, 1)

    def test_guest_sizing_overrides_only_when_different_and_reads_last_value(self):
        default = {"cores": 2, "memory_mib": 1024}
        self.assertEqual(sizing_args(default, default), [])
        args = sizing_args({"cores": 1, "memory_mib": 512}, default)
        self.assertEqual(args[:4], ["-m", "512", "-smp", "1"])
        self.assertIn("memory-backend=cas-guest-mem", args)
        argv = ["qemu", "-m", "1024", "-smp", "2", "-machine", "x", *args]
        self.assertEqual(effective_sizing(argv), {"cores": 1, "memory_mib": 512})
        with self.assertRaises(ValueError):
            effective_sizing(["qemu", "-smp", "2"])
        with self.assertRaises(ValueError):
            sizing_args({"cores": 1, "memory_mib": 64}, default)

    def test_fio_aggregate_sums_throughput_and_merges_histograms(self):
        def job(iops, bins):
            return {
                "jobs": [
                    {
                        "error": 0,
                        "read": {
                            "io_bytes": 4096 * sum(bins.values()),
                            "total_ios": sum(bins.values()),
                            "iops": iops,
                            "bw_bytes": iops * 4096,
                            "runtime": 5000,
                            "lat_ns": {"min": 1, "max": 9, "bins": bins},
                            "clat_ns": {"min": 1, "max": 9, "bins": bins},
                        },
                        "write": {
                            "io_bytes": 0,
                            "total_ios": 0,
                            "iops": 0,
                            "bw_bytes": 0,
                            "runtime": 0,
                            "lat_ns": {},
                            "clat_ns": {},
                        },
                    }
                ]
            }

        fio = aggregate_fio([job(10, {"1000": 5, "2000": 5}), job(30, {"2000": 90})])
        self.assertEqual(fio["guests"], 2)
        self.assertEqual(fio["read"]["iops"], 40)
        self.assertEqual(fio["read"]["total_ios"], 100)
        self.assertEqual(fio["read"]["lat_ns"]["bins"], {"1000": 5, "2000": 95})
        self.assertEqual(fio["read"]["lat_ns"]["percentile"]["50.000000"], 2000)
        self.assertEqual(fio["read"]["lat_ns"]["percentile"]["99.900000"], 2000)
        self.assertNotIn("lat_ns", fio["write"])
        failed = job(1, {"1000": 1})
        failed["jobs"][0]["error"] = 5
        with self.assertRaises(ValueError):
            aggregate_fio([failed])

    def test_burst_summary_requires_marker_and_computes_read_fraction(self):
        guest = {
            "name": "burst-0",
            "image": CLONE,
            "launched_ns": 1000,
            "ready_ns": 3000,
            "ssh_banner_ns": 4000,
            "first_command_ns": 5000,
            "probe_ns": 6000,
            "probe": "cas burst image 0 marker\n"
            "40 0 320 7 0 0 0 0 0 0 0\n"
            "32768 32000 4096\n",
        }
        summary = burst_guest_summary(guest, "cas burst image 0 marker")
        self.assertEqual(summary["first_command_ns"], 4000)
        self.assertEqual(summary["marker_ns"], 5000)
        self.assertEqual(summary["used_chunks"], 768)
        self.assertEqual(summary["read_chunks_at_marker"], 40)
        self.assertAlmostEqual(summary["read_fraction_at_marker"], 40 / 768)
        with self.assertRaises(ValueError):
            burst_guest_summary(guest, "cas burst image 1 marker")
        guest["probe"] = "cas burst image 0 marker\n1 2 3\n"
        with self.assertRaises(ValueError):
            burst_guest_summary(guest, "cas burst image 0 marker")

    def test_peer_counter_deltas_cover_has_and_peak_and_reject_resets(self):
        def peer(gets, has, inflight):
            value = report()
            value["peer"]["counters"].update(
                {
                    key: 0
                    for key in (
                        "connections",
                        "server_missing",
                        "server_errors",
                        "completed_reads",
                        "failed_reads",
                        "inline_attempts",
                        "inline_completed",
                    )
                }
            )
            value["peer"]["counters"].update(
                {"gets": gets, "has": has, "max_inflight": inflight}
            )
            value["peer"]["get_wire"] = {"sent": gets * 24, "received": gets * 4120}
            return value

        delta = peer_counters(peer(5, 1, 2), peer(12, 4, 9))
        self.assertEqual((delta["gets"], delta["has"]), (7, 3))
        self.assertEqual(delta["max_inflight"], {"before": 2, "after": 9})
        self.assertEqual(delta["get_wire"]["received"], 7 * 4120)
        with self.assertRaises(ValueError):
            peer_counters(peer(12, 4, 9), peer(5, 1, 2))

    def test_burst_boot_launches_every_guest_before_waiting(self):
        agent = Agent({})
        events = []
        try:
            with (
                patch.object(
                    Agent,
                    "launch",
                    lambda self, name, image, backend, format, sizing: (
                        events.append(("launch", name)),
                        {"pid": 1, "port": 2, "launched_ns": len(events)},
                    )[1],
                ),
                patch.object(
                    Agent,
                    "await_guest",
                    lambda self, name: (
                        events.append(("await", name)),
                        {"first_command_ns": 7},
                    )[1],
                ),
                patch.object(Agent, "shell", lambda self, guest, script: "probe"),
            ):
                result = agent.boot_burst(
                    [
                        {
                            "name": "g0",
                            "image": "a",
                            "backend": "cas",
                            "format": False,
                            "sizing": {},
                        },
                        {
                            "name": "g1",
                            "image": "b",
                            "backend": "cas",
                            "format": False,
                            "sizing": {},
                        },
                    ],
                    probe="cat marker",
                )
            self.assertEqual(events[:2], [("launch", "g0"), ("launch", "g1")])
            self.assertEqual(
                {e for e in events[2:]}, {("await", "g0"), ("await", "g1")}
            )
            self.assertEqual([g["probe"] for g in result], ["probe", "probe"])
            self.assertTrue(all("probe_ns" in g and g["image"] for g in result))
            with self.assertRaises(ValueError):
                agent.boot_burst([])
        finally:
            agent.close()

    def test_seeded_payload_carries_marker_and_shares_identical_content(self):
        with tempfile.TemporaryDirectory() as directory:
            agent = Agent({})
            try:
                agent.case = Path(directory)
                for guest in ("g0", "g1"):
                    (agent.case / guest).mkdir()
                written = {}

                def shell(guest, script):
                    name = script.split("of=/mnt/cas/")[1].split()[0]
                    data = (agent.case / guest / f"{name}-payload").read_bytes()
                    written[guest, name] = data
                    return hashlib.sha256(data).hexdigest() + "  " + name

                with patch.object(Agent, "shell", lambda self, g, s: shell(g, s)):
                    base0 = agent.write_payload("g0", "base", 7, 65536)
                    base1 = agent.write_payload("g1", "base", 7, 65536)
                    unique = agent.write_payload(
                        "g1", "unique", 8, 65536, marker="marker text"
                    )
                self.assertEqual(base0["sha256"], base1["sha256"])
                self.assertEqual(written["g0", "base"], written["g1", "base"])
                self.assertEqual(
                    (agent.case / "g0/base-payload").stat().st_ino,
                    (agent.case / "g1/base-payload").stat().st_ino,
                )
                block = written["g1", "unique"][:4096]
                self.assertEqual(block.rstrip(b"\0"), b"marker text")
                self.assertNotEqual(unique["sha256"], base1["sha256"])
                with self.assertRaises(ValueError):
                    agent.write_payload("g0", "odd", 1, 4097)
            finally:
                agent.close()

    def test_block_stat_parsing_rejects_short_lines(self):
        stat = parse_block_stat("8 0 64 3 1 0 8 2 0 5 5")
        self.assertEqual((stat["read_ios"], stat["read_sectors"]), (8, 64))
        with self.assertRaises(ValueError):
            parse_block_stat("1 2 3")
