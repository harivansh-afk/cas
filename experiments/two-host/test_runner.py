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
    checked,
    effective_sizing,
    fio_options,
    image_id,
    parse_block_stat,
    sizing_args,
    ssh_ready,
    working_set_script,
)
from runner import (
    DEFAULT_CASES,
    RING_RETENTION,
    aggregate_fio,
    apply_overrides,
    burst_guest_summary,
    classify,
    deltas,
    first_remote_read,
    first_useful_io,
    has_overlap,
    normalize_settings,
    peer_clock_origin,
    peer_counters,
    phases,
    plan_cases,
    rsync_bytes,
    working_set_summary,
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
                patch.object(Agent, "report", lambda self: {"peer": {"recent": []}}),
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
                    sample_ring=True,
                )
            self.assertEqual(events[:2], [("launch", "g0"), ("launch", "g1")])
            self.assertEqual(
                {e for e in events[2:]}, {("await", "g0"), ("await", "g1")}
            )
            self.assertEqual([g["probe"] for g in result], ["probe", "probe"])
            self.assertTrue(all("probe_ns" in g and g["image"] for g in result))
            # The ring sample follows the probe so marker timing stays unperturbed.
            self.assertTrue(
                all(g["report_at_ready_ns"] >= g["probe_ns"] for g in result)
            )
            self.assertEqual(result[0]["report_at_ready"], {"peer": {"recent": []}})
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


def ring_record(id, received, completed, success=True):
    return {
        "id": id,
        "chunks": 1,
        "received_ns": received,
        "success": success,
        "phases": {
            "send_ns": received + 1,
            "first_byte_ns": received + 2,
            "verified_ns": completed - 1,
        },
        "completed_ns": completed,
    }


def ring_sample(records, retention=RING_RETENTION):
    return {"peer": {"recent": records, "retention": retention}}


class WorkingSetTests(unittest.TestCase):
    """Working-set configuration and the handoff/boot/first-IO arithmetic."""

    def test_working_set_defaults_empty_and_is_inherited_per_case(self):
        settings = normalize_settings(config())
        empty = {"commands": [], "chroot": None, "cwd": None, "env": {}}
        self.assertEqual(settings["working_set"], empty)
        self.assertEqual(settings["burst"]["working_set"], empty)
        self.assertEqual(settings["resume"]["working_set"], empty)
        commands = ["dd if=/mnt/cas/data bs=4096 count=1 | sha256sum", "sha256sum /mnt/cas/data"]
        spec = {**empty, "commands": commands}
        shared = normalize_settings(config(working_set=commands))
        self.assertEqual(shared["burst"]["working_set"], spec)
        self.assertEqual(shared["resume"]["working_set"], spec)
        own = normalize_settings(
            config(working_set=commands, resume={"working_set": ["true"]})
        )
        self.assertEqual(own["resume"]["working_set"], {**empty, "commands": ["true"]})
        self.assertEqual(own["burst"]["working_set"], spec)
        cleared = normalize_settings(
            config(working_set=commands, burst={"working_set": []})
        )
        self.assertEqual(cleared["burst"]["working_set"], empty)
        cfg = apply_overrides(config(), ['working_set=["ls -l"]'])
        self.assertEqual(normalize_settings(cfg)["resume"]["working_set"]["commands"], ["ls -l"])

    def test_working_set_chroot_form(self):
        spec = {"commands": ["pwd"], "chroot": "/mnt/cas/root", "cwd": "/testbed", "env": {"HOME": "/root", "PATH": "/bin"}}
        settings = normalize_settings(config(working_set=spec))
        self.assertEqual(settings["working_set"], spec)
        self.assertEqual(settings["burst"]["working_set"], spec)
        inherited = normalize_settings(config(working_set=spec, resume={"working_set": {"commands": ["ls"]}}))
        self.assertEqual(inherited["resume"]["working_set"], {"commands": ["ls"], "chroot": None, "cwd": None, "env": {}})
        for bad in (
            {"commands": ["pwd"], "cwd": "/testbed"},
            {"commands": ["pwd"], "chroot": "relative"},
            {"commands": ["pwd"], "chroot": "/a'b"},
            {"commands": ["pwd"], "chroot": "/r", "env": {"1BAD": "x"}},
            {"commands": ["pwd"], "chroot": "/r", "env": {"OK": 1}},
            {"commands": ["pwd"], "unknown": 1},
        ):
            with self.subTest(bad=bad), self.assertRaises((ValueError, TypeError)):
                normalize_settings(config(working_set=bad))

    def test_working_set_script_wraps_chroot_commands(self):
        self.assertEqual(working_set_script("ls"), "cd /mnt/cas\nls")
        script = working_set_script("echo $HOME > out", "/mnt/cas/root", "/testbed", {"PATH": "/bin", "HOME": "/root"})
        self.assertEqual(
            script,
            "env -i HOME=/root PATH=/bin chroot /mnt/cas/root /bin/bash -lc 'cd /testbed && echo $HOME > out'",
        )
        self.assertEqual(working_set_script("ls", "/r"), "env -i chroot /r /bin/bash -lc ls")

    def test_working_set_rejects_non_command_lists(self):
        for bad in (
            {"working_set": "ls"},
            {"working_set": [1]},
            {"working_set": [""]},
            {"working_set": ["ls\0"]},
            {"working_set": ["true"] * 257},
            {"resume": {"working_set": "ls"}},
            {"burst": {"working_set": [None]}},
        ):
            with self.subTest(bad=bad), self.assertRaises((ValueError, TypeError)):
                normalize_settings(config(**bad))

    def test_peer_clock_origin_intersects_only_single_record_reads(self):
        # Host clock: origin at 1_000; ring stamps are host minus origin.
        calibration = {
            "reads": [
                {
                    "started_ns": 5_000,
                    "completed_ns": 5_400,
                    "completed_reads": 1,
                    "records": [ring_record(1, 4_100, 4_300)],
                },
                {
                    "started_ns": 6_000,
                    "completed_ns": 6_500,
                    "completed_reads": 0,
                    "records": [],
                },
                {
                    "started_ns": 7_000,
                    "completed_ns": 7_300,
                    "completed_reads": 1,
                    "records": [ring_record(2, 6_050, 6_250)],
                },
            ]
        }
        origin = peer_clock_origin(calibration)
        # read 1: [5000-4100, 5400-4300] = [900, 1100]; read 3: [950, 1050].
        self.assertEqual((origin["lower_ns"], origin["upper_ns"]), (950, 1050))
        self.assertEqual((origin["remote_reads"], origin["reads"]), (2, 3))
        local_only = {"reads": [calibration["reads"][1]]}
        with self.assertRaises(ValueError):
            peer_clock_origin(local_only)
        ambiguous = {"reads": [{**calibration["reads"][0], "completed_reads": 2}]}
        with self.assertRaises(ValueError):
            peer_clock_origin(ambiguous)
        disjoint = {
            "reads": [
                calibration["reads"][0],
                {
                    "started_ns": 9_000,
                    "completed_ns": 9_100,
                    "completed_reads": 1,
                    "records": [ring_record(3, 4_000, 4_050)],
                },
            ]
        }
        with self.assertRaises(ValueError):
            peer_clock_origin(disjoint)

    def test_first_remote_read_takes_the_earliest_sample_and_reports_eviction(self):
        before = [ring_record(1, 10, 20), ring_record(2, 30, 40)]
        ready = ring_sample(before + [ring_record(3, 90, 95), ring_record(4, 70, 80)])
        later = ring_sample([ring_record(5, 60, 65)])
        self.assertEqual(
            first_remote_read([ready, later], since=40),
            {"received_ns": 70, "censored": False},
        )
        self.assertEqual(
            first_remote_read([ring_sample(before)], since=40)["received_ns"], None
        )
        # Full retention holding only post-anchor records may have evicted the first.
        full = ring_sample([ring_record(i, 100 + i, 200 + i) for i in range(4)], 4)
        self.assertTrue(first_remote_read([full, later], since=40)["censored"])
        # A pre-anchor record still present proves nothing after it was evicted.
        kept = ring_sample(
            [ring_record(0, 10, 20)]
            + [ring_record(i, 100 + i, 200 + i) for i in range(1, 4)],
            4,
        )
        self.assertEqual(first_remote_read([kept], since=40)["received_ns"], 101)

    def test_first_useful_io_converts_through_origin_bounds_or_stays_null(self):
        origin = {"lower_ns": 950, "upper_ns": 1050}
        first = {"received_ns": 4_100, "censored": False}
        interval = first_useful_io(first, origin, anchor_ns=3_000)
        # Host time of the read is 4_100 + [950, 1050]; anchor 3_000.
        self.assertEqual((interval["lower_ns"], interval["upper_ns"]), (2050, 2150))
        self.assertEqual(interval["ns"], 2100)
        self.assertFalse(interval["censored"])
        missing = first_useful_io({"received_ns": None, "censored": True}, origin, 0)
        self.assertIsNone(missing["ns"])
        self.assertTrue(missing["censored"])
        uncalibrated = first_useful_io(first, {"unavailable": "no remote"}, 0)
        self.assertIsNone(uncalibrated["ns"])
        self.assertEqual(uncalibrated["reason"], "no remote")

    def test_working_set_summary_measures_from_each_guest_anchor(self):
        replay = [
            {
                "name": "g0",
                "commands": [
                    {"index": 0, "started_ns": 50, "completed_ns": 60},
                    {"index": 1, "started_ns": 61, "completed_ns": 90},
                ],
            },
            {
                "name": "g1",
                "commands": [{"index": 0, "started_ns": 70, "completed_ns": 100}],
            },
        ]
        rows = working_set_summary(replay, {"g0": 10, "g1": 20})
        self.assertEqual(rows[0]["time_to_working_set_ns"], 80)
        self.assertEqual(rows[0]["replay_ns"], 40)
        self.assertEqual(rows[0]["per_command_completion_ns"], [50, 80])
        self.assertEqual(rows[1]["time_to_working_set_ns"], 80)
        replay[1]["commands"][0]["completed_ns"] = 69
        with self.assertRaises(ValueError):
            working_set_summary(replay, {"g0": 10, "g1": 20})
        with self.assertRaises(ValueError):
            working_set_summary([{"name": "g2", "commands": []}], {"g2": 0})

    def test_checked_accepts_a_per_call_timeout(self):
        self.assertEqual(checked([sys.executable, "-c", "print(1)"], timeout=30), "1\n")
        agent = Agent({})
        try:
            agent.guests = {
                "g": [sys.executable, "-c", "import sys; print(sys.argv[1])"]
            }
            self.assertEqual(
                agent.shell("g", "echo hi", timeout=30), "sh -eu -c 'echo hi'\n"
            )
        finally:
            agent.close()

    def test_agent_replays_commands_in_order_from_the_data_disk(self):
        with tempfile.TemporaryDirectory() as directory:
            agent = Agent({})
            try:
                agent.case = Path(directory)
                for guest in ("g0", "g1"):
                    (agent.case / guest).mkdir()
                scripts = []

                def shell(self, guest, script, timeout=600):
                    scripts.append((guest, script, timeout))
                    return f"{guest}:{script.splitlines()[-1]}\n"

                with patch.object(Agent, "shell", shell):
                    result = agent.run_commands(
                        ["g0", "g1"], ["sha256sum data", "wc -c < unique"], "cold"
                    )
                for guest, script, timeout in scripts:
                    self.assertTrue(script.startswith("cd /mnt/cas\n"))
                    self.assertEqual(timeout, 1800)
                self.assertEqual(
                    [s[1].splitlines()[-1] for s in scripts if s[0] == "g0"],
                    ["sha256sum data", "wc -c < unique"],
                )
                self.assertEqual([g["name"] for g in result], ["g0", "g1"])
                rows = result[0]["commands"]
                self.assertEqual([r["index"] for r in rows], [0, 1])
                self.assertLessEqual(rows[0]["completed_ns"], rows[1]["started_ns"])
                self.assertEqual(
                    (agent.case / "g0/working-set-cold-1.out").read_text(),
                    "g0:wc -c < unique\n",
                )
                self.assertEqual(
                    rows[1]["stdout_sha256"],
                    hashlib.sha256(b"g0:wc -c < unique\n").hexdigest(),
                )
                with self.assertRaises(ValueError):
                    agent.run_commands(["g0"], [], "cold")
                with self.assertRaises(ValueError):
                    agent.run_commands(["g0"], ["true"], "bad label")
            finally:
                agent.close()

    def test_agent_calibration_brackets_reads_and_keeps_local_hits(self):
        agent = Agent({})
        try:
            reports = iter(
                [
                    {"peer": {"recent": [], "counters": {"completed_reads": 0}}},
                    {
                        "peer": {
                            "recent": [ring_record(7, 100, 150)],
                            "counters": {"completed_reads": 1},
                        }
                    },
                    {
                        "peer": {
                            "recent": [ring_record(7, 100, 150)],
                            "counters": {"completed_reads": 1},
                        }
                    },
                ]
            )
            reads = []
            with (
                patch.object(Agent, "report", lambda self: next(reports)),
                patch.object(
                    Agent, "shell", lambda self, g, s, timeout=600: reads.append(s)
                ),
            ):
                result = agent.calibrate_ring("g0", "/mnt/cas/data", reads=2)
            self.assertEqual(len(result["reads"]), 2)
            self.assertIn("skip=0 ", reads[0])
            self.assertIn("skip=4 ", reads[1])
            self.assertEqual(result["reads"][0]["records"][0]["id"], 7)
            self.assertEqual(result["reads"][0]["completed_reads"], 1)
            self.assertEqual(result["reads"][1]["records"], [])
            self.assertLessEqual(
                result["reads"][0]["started_ns"], result["reads"][0]["completed_ns"]
            )
            with self.assertRaises(ValueError):
                agent.calibrate_ring("g0", "data", reads=1)
        finally:
            agent.close()
