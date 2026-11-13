"""Pure-Python checks of the campaign driver's plan loader, config validation,
pinning rules and resume logic. Run with
`uv run --no-project python -m unittest discover -s experiments/cloudlab`."""

import argparse
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import campaign  # noqa: E402
import campaignlib as lib  # noqa: E402

HERE = Path(__file__).resolve().parent


def arm(**overrides):
    base = {
        "arm": "t",
        "runner": "two-host",
        "description": "test",
        "pinning": {"x": {"logical_cpus": 8, "irq": "0", "daemon": ["0-1", "2-3"], "controller": "2-3", "guests": "4-7"}},
        "config": {"settings": {"guest": {"cores": 1, "memory_mib": 512}, "burst": {"images": 4}}, "cases": ["reads-memory", "reads-storage-g4", "burst-images", "resume-elsewhere"]},
    }
    base.update(overrides)
    return base


def results(p50s, success=True):
    cohorts = [
        {"name": f"r{i}-random-qd1", "load": "quiet", "fio": {"read": {"clat_ns": {"percentile": {"50.000000": p}}}}}
        for i, p in enumerate(p50s)
    ]
    cohorts.append({"name": "r0-sequential-qd1", "load": "quiet", "fio": {"read": {"clat_ns": {"percentile": {"50.000000": 1}}}}})
    cohorts.append({"name": "r0-contention", "load": "compaction-shipping", "fio": {"read": {"clat_ns": {"percentile": {"50.000000": 10**9}}}}})
    return {"success": success, "cases": {"reads-memory": {"cohorts": cohorts}}}


class CpuSets(unittest.TestCase):
    def test_parse_and_format(self):
        self.assertEqual(lib.parse_cpuset("2-5,8, 1"), [1, 2, 3, 4, 5, 8])
        self.assertEqual(lib.format_cpuset([1, 2, 3, 4, 5, 8]), "1-5,8")
        for bad in ("", "a", "5-2", "1,,2", 3):
            with self.assertRaises(ValueError):
                lib.parse_cpuset(bad)


class Plan(unittest.TestCase):
    def test_shapes(self):
        plan = lib.load_plan([["frozen", "reads-memory", 5], {"arm": "fast", "case": "reads-memory", "repetitions": 2}])
        self.assertEqual(len(plan), 1)
        self.assertEqual(plan[0].units[1], lib.Unit("fast", "reads-memory", 2))
        days = lib.load_plan({"days": [{"day": 1, "units": [["r0", "floor", 1]]}, {"units": [["fast", "x", 3]]}]})
        self.assertEqual([d.day for d in days], [1, 2])

    def test_rejections(self):
        for bad in (
            [],
            [["frozen", "reads-memory"]],
            [["Frozen", "reads-memory", 1]],
            [["frozen", "reads-memory", 0]],
            [["frozen", "reads-memory", 21]],
            [["frozen", "reads-memory", True]],
            [["frozen", "reads-memory", 1], ["frozen", "reads-memory", 2]],
            {"days": [{"day": 2, "units": [["a", "b", 1]]}]},
            {"days": [{"day": 1, "units": []}]},
        ):
            with self.assertRaises(ValueError, msg=repr(bad)):
                lib.load_plan(bad)

    def test_rotation(self):
        plan = lib.load_plan([["a", "c", 3], ["b", "c", 3], ["d", "c", 1]])
        rows = [(u.arm, r) for _, u, r in lib.schedule(plan)]
        self.assertEqual(rows, [("a", 1), ("b", 1), ("d", 1), ("b", 2), ("a", 2), ("a", 3), ("b", 3)])
        self.assertEqual(lib.unit_id(2, plan[0].units[0], 3), "d2-a-c-r3")

    def test_validate_against_arms(self):
        arms = {"t": arm()}
        plan = lib.load_plan([["t", "reads-memory", 1], ["t", "nope", 1], ["z", "x", 1]])
        problems = lib.validate_plan(plan, arms)
        self.assertEqual(len(problems), 2)
        self.assertIn("no case 'nope'", problems[0])
        self.assertIn("unknown arm 'z'", problems[1])


class Arms(unittest.TestCase):
    def test_checked_in_arms_load(self):
        arms = lib.load_arms()
        for name in ("frozen", "fast", "raced", "warm-owner", "burst", "resume", "native-raw", "native-zfs", "native-cas", "r0"):
            self.assertIn(name, arms)
        for name, a in arms.items():
            self.assertEqual(set(a["pinning"]) >= {"r6615", "c6525-25g"}, True, name)
        self.assertIn("RACE_BUDGET_US", lib.unresolved(arms["raced"]["config"]))
        self.assertIn("WORKING_SET_FROM_BUNDLE", lib.unresolved(arms["burst"]["config"]))
        self.assertIn("WORKING_SET_FROM_BUNDLE", lib.unresolved(arms["resume"]["config"]))
        self.assertEqual(arms["frozen"]["config"]["settings"]["fast_settled_reads"], False)
        self.assertIsNone(arms["frozen"]["config"]["settings"]["peer_race_us"])
        self.assertEqual(arms["frozen"]["config"]["settings"]["burst"]["owner_cache_bytes"], 0)
        self.assertEqual(arms["raced"]["requires_runner_settings"], ["fast_settled_reads", "peer_race_us"])
        self.assertEqual(arms["burst"]["config"]["settings"]["burst"]["images"], 16)
        plan = lib.load_plan(HERE / "plans" / "three-days.json")
        self.assertEqual(lib.validate_plan(plan, arms), [])
        self.assertEqual(lib.validate_plan(lib.load_plan(HERE / "plans" / "dev-loopback.json"), arms), [])

    def test_rejections(self):
        for bad in (
            arm(runner="other"),
            arm(pinning={}),
            arm(pinning={"x": {"logical_cpus": 8, "irq": "0", "daemon": "1", "controller": "2-3"}}),
            arm(pinning={"x": {"logical_cpus": 1, "irq": "0", "daemon": "1", "controller": "2-3", "guests": "4"}}),
            arm(config={"settings": {}, "cases": ["unknown-case"]}),
            arm(config={"settings": {}, "cases": []}),
            arm(config={"settings": {}, "cases": ["reads-memory"]}, requires_runner_settings=["fast_settled_reads"]),
            arm(config={"settings": {}, "cases": ["reads-memory"]}, requires_runner_settings=["bogus"]),
            arm(runner="native", backend="ext4", cases={"g1": {"guests": 1}}),
            arm(runner="native", backend="raw", cases={"g1": {"guests": 0}}),
            arm(runner="native", backend="raw", cases={"g1": {"guests": 1, "script_args": [1]}}),
            arm(runner="r0", cases=["floor", "x"]),
        ):
            with self.assertRaises(ValueError, msg=json.dumps(bad)[:80]):
                lib.validate_arm(bad)

    def test_case_guests_and_cores(self):
        a = arm()
        self.assertEqual(lib.case_guests(a, "reads-memory"), 1)
        self.assertEqual(lib.case_guests(a, "reads-storage-g4"), 4)
        self.assertEqual(lib.case_guests(a, "burst-images"), 4)
        self.assertIsNone(lib.case_guests(a, "nothing"))
        self.assertEqual(lib.guest_cores(a, "reads-memory"), 1)
        native = arm(runner="native", backend="raw", cases={"g1": {"guests": 1}})
        self.assertEqual(lib.case_guests(native, "g1"), 1)
        self.assertEqual(lib.guest_cores(native, "g1"), 2)


class Pinning(unittest.TestCase):
    SETS = {"logical_cpus": 8, "irq": "0", "daemon": ["0-1", "2-3"], "controller": "2-3", "guests": "4-7"}

    def test_fits(self):
        self.assertEqual(lib.check_pinning(self.SETS, 0, 8, 1, 4), [])
        self.assertEqual(lib.check_pinning(self.SETS, 1, 8, 1, 4), [])

    def test_problems(self):
        problems = lib.check_pinning(self.SETS, 0, 4, 1, 4)
        self.assertTrue(any("exceeds the 4 logical cpus" in p for p in problems))
        self.assertTrue(any("inventory has 4" in p for p in problems))
        problems = lib.check_pinning(self.SETS, 0, 8, 1, 8)
        self.assertTrue(any("need 8 guest cpus, set has 4" in p for p in problems))
        self.assertTrue(any("budget" in p for p in problems))
        overlap = dict(self.SETS, daemon="4-5")
        self.assertTrue(any("overlap" in p for p in lib.check_pinning(overlap, 0, 8, 1, 1)))
        narrow = dict(self.SETS, controller="2")
        self.assertTrue(any("controller" in p for p in lib.check_pinning(narrow, 0, 8, 1, 1)))

    def test_checked_in_sets_fit_their_node_types(self):
        arms = lib.load_arms()
        for name, a in arms.items():
            for node_type, sets in a["pinning"].items():
                for case in lib.arm_cases(a):
                    guests = lib.case_guests(a, case)
                    if a["runner"] == "native" and guests > lib.NATIVE_MAX_GUESTS:
                        continue
                    problems = lib.check_pinning(sets, 0, sets["logical_cpus"], lib.guest_cores(a, case), guests)
                    if node_type.startswith("loopback") and guests > 4:
                        self.assertTrue(problems, f"{name} {node_type} {case} should not fit 8 vCPUs")
                    else:
                        self.assertEqual(problems, [], f"{name} {node_type} {case}")


class Placeholders(unittest.TestCase):
    def test_substitute(self):
        value = {"a": "RACE_BUDGET_US", "b": ["WORKING_SET_FROM_BUNDLE", 1], "c": "RACE_BUDGET_US_X"}
        self.assertEqual(sorted(lib.unresolved(value)), ["RACE_BUDGET_US", "WORKING_SET_FROM_BUNDLE"])
        filled = lib.substitute(value, {"RACE_BUDGET_US": 70, "WORKING_SET_FROM_BUNDLE": ["ls"]})
        self.assertEqual(filled, {"a": 70, "b": [["ls"], 1], "c": "RACE_BUDGET_US_X"})
        self.assertEqual(lib.unresolved(filled), [])

    def test_working_set_from_bundle(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "actions.json"
            path.write_text(json.dumps([{"command": f"echo {i}", "recorded_returncode": 0} for i in range(12)]))
            ws = lib.working_set_from_bundle(path)
            self.assertEqual(ws["commands"], [f"echo {i}" for i in range(10)])
            self.assertEqual(ws["total_actions"], 12)
            self.assertEqual(len(ws["sha256"]), 64)
            path.write_text(json.dumps([{"command": ""}]))
            with self.assertRaises(ValueError):
                lib.working_set_from_bundle(path)
            path.write_text("[]")
            with self.assertRaises(ValueError):
                lib.working_set_from_bundle(path)

    def test_race_budget(self):
        us, p50s = lib.race_budget_us(results([70_000, 90_000, 80_000]))
        self.assertEqual((us, p50s), (80, [70_000, 90_000, 80_000]))
        self.assertEqual(lib.race_budget_us(results([400]))[0], 1)
        with self.assertRaises(ValueError):
            lib.race_budget_us(results([70_000], success=False))
        with self.assertRaises(ValueError):
            lib.race_budget_us({"success": True, "cases": {"reads-memory": {"cohorts": []}}})


class Resume(unittest.TestCase):
    def test_unit_state(self):
        with tempfile.TemporaryDirectory() as tmp:
            unit = Path(tmp) / "u"
            self.assertEqual(lib.unit_state(unit), "absent")
            unit.mkdir()
            self.assertEqual(lib.unit_state(unit), "partial")
            (unit / "controller").mkdir()
            (unit / "controller" / "results.json").write_text('{"success": false}')
            self.assertEqual(lib.unit_state(unit), "failed")
            (unit / "controller" / "results.json").write_text('{"success": true}')
            self.assertEqual(lib.unit_state(unit), "done")
            (unit / "controller" / "results.json").write_text("{broken")
            self.assertEqual(lib.unit_state(unit), "failed")
            native = Path(tmp) / "n"
            (native / "native").mkdir(parents=True)
            (native / "native" / "outcome.json").write_text('{"success": true}')
            self.assertEqual(lib.unit_state(native), "done")

    def test_decide(self):
        self.assertEqual(lib.decide("absent", False), "run")
        self.assertEqual(lib.decide("done", False), "skip")
        self.assertEqual(lib.decide("failed", False), "refuse")
        self.assertEqual(lib.decide("partial", False), "refuse")
        self.assertEqual(lib.decide("done", True), "run")
        self.assertEqual(lib.decide("partial", True), "run")

    def test_driver_step_skips_refuses_and_archives(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            nodes = {
                "campaign": "c",
                "loopback": True,
                "nodes": [
                    {"name": "a", "node_type": "loopback-8", "address": "127.0.0.1", "source": "/s", "runner": "/r", "storage_mount": "/m", "output_root": str(root)},
                    {"name": "b", "node_type": "loopback-8", "address": "127.0.0.1", "source": "/s", "runner": "/r", "storage_mount": "/m", "output_root": str(root)},
                ],
            }
            (root / "nodes.json").write_text(json.dumps(nodes))
            (root / "plan.json").write_text(json.dumps([["fast", "reads-memory", 2], ["frozen", "reads-memory", 1]]))
            args = argparse.Namespace(nodes=str(root / "nodes.json"), plan=str(root / "plan.json"), arms=str(lib.ARMS_DIR), export=None, redo=False, dry_run=False)
            c = campaign.Campaign(args)
            c.dir.mkdir(parents=True)
            c.units_dir.mkdir()
            c.manifest_data = {"evidence_label": "development", "fills": {}}
            calls = []

            def fake_run_unit(day, unit, rep, uid):
                calls.append(uid)
                (c.units_dir / uid / "controller").mkdir(parents=True)
                (c.units_dir / uid / "controller" / "results.json").write_text(json.dumps({"success": uid.endswith("r1")}))
                return {"success": uid.endswith("r1"), "seconds": 1}

            with patch.object(c, "run_unit", side_effect=fake_run_unit), patch.object(c, "export"):
                for day, unit, rep in c.schedule:
                    c.step(day, unit, rep, lib.unit_id(day, unit, rep))
                self.assertEqual(calls, ["d1-fast-reads-memory-r1", "d1-frozen-reads-memory-r1", "d1-fast-reads-memory-r2"])
                states = {k: v["state"] for k, v in c.status["units"].items()}
                self.assertEqual(states, {"d1-fast-reads-memory-r1": "done", "d1-frozen-reads-memory-r1": "done", "d1-fast-reads-memory-r2": "failed"})
                # Second pass: done units are kept, the failed one is refused without --redo.
                calls.clear()
                for day, unit, rep in c.schedule:
                    c.step(day, unit, rep, lib.unit_id(day, unit, rep))
                self.assertEqual(calls, [])
                self.assertEqual(c.status["units"]["d1-fast-reads-memory-r2"]["state"], "refused")
                # --redo reruns it and keeps the old outputs as an attempt.
                c.args.redo = True
                c.step(1, lib.Unit("fast", "reads-memory", 2), 2, "d1-fast-reads-memory-r2")
                self.assertEqual(calls, ["d1-fast-reads-memory-r2"])
                self.assertTrue((c.units_dir / "d1-fast-reads-memory-r2.attempt-1" / "controller" / "results.json").exists())
            lines = lib.status_lines(c.status)
            self.assertTrue(any("units:" in line for line in lines))
            self.assertLessEqual(len(lines), 40)


class Build(unittest.TestCase):
    def test_wrapper_paths(self):
        text = 'export PYTHONDONTWRITEBYTECODE=1; exec python3 /nix/store/abc-two-host/runner.py --build-info /nix/store/def-cas-two-host-build.json "$@"'
        self.assertEqual(lib.wrapper_paths(text), {"build_info": "/nix/store/def-cas-two-host-build.json", "runner": "/nix/store/abc-two-host/runner.py"})
        self.assertEqual(lib.wrapper_paths("nothing")["runner"], None)

    def test_runner_supports(self):
        self.assertEqual(lib.runner_supports('DEFAULTS = {"inline_us": 50}'), {"fast_settled_reads": False, "peer_race_us": False})
        self.assertEqual(lib.runner_supports('"peer_race_us": None,\n"fast_settled_reads": False,'), {"fast_settled_reads": True, "peer_race_us": True})

    def test_two_host_config(self):
        a = arm()
        nodes = [
            {"name": "a", "node_type": "x", "address": "10.0.0.1", "storage_mount": "/mnt/x", "output_root": "/out", "zfs_parent": "tank/x"},
            {"name": "b", "node_type": "x", "address": "10.0.0.2", "storage_mount": "/mnt/y", "output_root": "/out", "ssh": "u@b", "runner": "/r"},
        ]
        config = lib.two_host_config(a, nodes, "camp", "d1-t-reads-memory-r1", "reads-memory", {})
        self.assertEqual(config["cases"], ["reads-memory"])
        self.assertEqual(config["nodes"][0]["storage"], "/mnt/x/camp/d1-t-reads-memory-r1")
        self.assertEqual(config["nodes"][1]["output"], "/out/camp/units/d1-t-reads-memory-r1/host-b")
        self.assertEqual((config["nodes"][0]["cpus"], config["nodes"][1]["cpus"]), ("0-1", "2-3"))
        self.assertEqual(config["nodes"][1]["ssh"], "u@b")
        self.assertNotIn("ssh", config["nodes"][0])
        self.assertEqual(config["nodes"][0]["zfs_port"], 17487)

    def test_validate_nodes(self):
        good = {"campaign": "c", "nodes": [
            {"name": "a", "node_type": "r6615", "address": "1", "source": "/s", "runner": "/r", "storage_mount": "/m", "output_root": "/o"},
            {"name": "b", "node_type": "r6615", "address": "2", "source": "/s", "runner": "/r", "storage_mount": "/m", "output_root": "/o", "ssh": "u@b"},
        ]}
        self.assertEqual(len(lib.validate_nodes(good)), 2)
        no_ssh = json.loads(json.dumps(good))
        del no_ssh["nodes"][1]["ssh"]
        with self.assertRaises(ValueError):
            lib.validate_nodes(no_ssh)
        no_ssh["loopback"] = True
        self.assertEqual(len(lib.validate_nodes(no_ssh)), 2)
        relative = json.loads(json.dumps(good))
        relative["nodes"][0]["storage_mount"] = "mnt"
        with self.assertRaises(ValueError):
            lib.validate_nodes(relative)


if __name__ == "__main__":
    unittest.main()
