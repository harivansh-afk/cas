#!/usr/bin/env python3
"""CloudLab campaign driver: unattended, resumable, evidence-complete runs of
the arm configs under experiments/cloudlab/arms. See docs/cloudlab-campaign.md.

    campaign.py start   --nodes N.json --plan P.json [--export DEST] [--redo]
    campaign.py run     ... [--dry-run]     # foreground worker (start detaches it)
    campaign.py status  --nodes N.json
"""

import argparse
import hashlib
import json
import os
import shlex
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import campaignlib as lib  # noqa: E402

HERE = Path(__file__).resolve().parent
UNIT_TIMEOUT = 8 * 3600
R0_PORT = 17490


def now():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def sha256_file(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def write_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=1, sort_keys=True) + "\n")
    tmp.replace(path)


class Blocked(Exception):
    """A unit the built runners cannot execute as configured; never launched."""


class Campaign:
    def __init__(self, args):
        self.args = args
        self.nodes_path = Path(args.nodes).resolve()
        self.nodes_config = json.loads(self.nodes_path.read_text())
        self.nodes = lib.validate_nodes(self.nodes_config)
        self.loopback = lib.loopback(self.nodes_config)
        self.name = self.nodes_config["campaign"]
        self.dir = Path(self.nodes[0]["output_root"]) / self.name
        self.units_dir = self.dir / "units"
        self.export_path = args.export or self.nodes_config.get("export")
        self.plan_path = Path(args.plan).resolve() if args.plan else None
        self.plan = lib.load_plan(self.plan_path) if self.plan_path else []
        self.arms = lib.load_arms(args.arms)
        problems = lib.validate_plan(self.plan, self.arms)
        if problems:
            raise SystemExit("plan problems:\n  " + "\n  ".join(problems))
        self.schedule = lib.schedule(self.plan)
        self.status_path = self.dir / "status.json"
        self.manifest_path = self.dir / "manifest.json"
        self.status = json.loads(self.status_path.read_text()) if self.status_path.exists() else {}
        self.mounts = {}

    # ------------------------------------------------------------ logging

    def log(self, message):
        line = f"{now()} {message}"
        print(line, file=sys.stderr, flush=True)
        with (self.dir / "campaign.log").open("a") as log:
            log.write(line + "\n")

    def set_status(self, **fields):
        self.status.update(fields, updated=now())
        write_json(self.status_path, self.status)

    def unit_status(self, uid, **fields):
        entry = self.status.setdefault("units", {}).setdefault(uid, {"state": "pending"})
        entry.update(fields)
        self.set_status()

    # ------------------------------------------------------------ nodes

    def shell(self, node, argv, timeout=600, check=True):
        """Run argv on a node (local, or over batch-mode ssh with a verified
        host key); log it; return stdout."""
        command = list(argv)
        if node.get("ssh"):
            command = ["ssh", "-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes", node["ssh"], shlex.join(argv)]
        start = time.monotonic()
        result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
        self.log(f"[{node['name']}] rc={result.returncode} {time.monotonic() - start:.1f}s {shlex.join(argv)[:160]}")
        if check and result.returncode:
            raise RuntimeError(f"[{node['name']}] {shlex.join(argv)[:120]} failed rc={result.returncode}: {result.stderr[-800:]}")
        return result.stdout

    def pull(self, node, remote_dir, local_dir):
        """Copy a remote node's directory into the campaign tree."""
        Path(local_dir).mkdir(parents=True, exist_ok=True)
        if not node.get("ssh"):
            if Path(remote_dir).resolve() != Path(local_dir).resolve() and Path(remote_dir).exists():
                shutil.copytree(remote_dir, local_dir, dirs_exist_ok=True)
            return
        subprocess.run(
            ["rsync", "-a", "-e", "ssh -o BatchMode=yes -o StrictHostKeyChecking=yes", f"{node['ssh']}:{remote_dir}/", f"{local_dir}/"],
            check=True, timeout=3600,
        )

    def script(self, node, name):
        return f"{node['source']}/experiments/cloudlab/{name}"

    def mount_type(self, node):
        if node["name"] not in self.mounts:
            out = self.shell(node, ["findmnt", "-J", "-T", node["storage_mount"], "-o", "FSTYPE,TARGET,SOURCE"], check=False)
            try:
                self.mounts[node["name"]] = json.loads(out)["filesystems"][0]
            except (ValueError, KeyError, IndexError):
                self.mounts[node["name"]] = {"fstype": None, "target": None, "source": None}
        return self.mounts[node["name"]]

    def exists(self, node, path, flag="-e"):
        return subprocess.run(
            self.wrap(node, ["test", flag, path]), capture_output=True, timeout=120
        ).returncode == 0

    def wrap(self, node, argv):
        if node.get("ssh"):
            return ["ssh", "-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes", node["ssh"], shlex.join(argv)]
        return list(argv)

    # ------------------------------------------------------------ inventory

    def inventories(self, phase):
        """Snapshot every node's inventory without changing anything."""
        result = {}
        for node in self.nodes:
            remote = f"{node['output_root']}/{self.name}/inventory-{phase}-{node['name']}.json"
            self.shell(node, ["bash", self.script(node, "setup.sh"), "--inventory-only", "--out", remote], timeout=900)
            local = self.dir / "inventory" / phase / f"{node['name']}.json"
            local.parent.mkdir(parents=True, exist_ok=True)
            if node.get("ssh"):
                subprocess.run(["scp", "-q", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes", f"{node['ssh']}:{remote}", str(local)], check=True, timeout=300)
            elif Path(remote).resolve() != local.resolve():
                shutil.copyfile(remote, local)
            result[node["name"]] = json.loads(local.read_text())
        self.set_status(**{f"inventory_{phase}": {n: sha256_file(self.dir / "inventory" / phase / f"{n}.json") for n in result}})
        return result

    # ------------------------------------------------------------ manifest

    def node_identity(self, node):
        revision = self.shell(node, ["git", "-C", node["source"], "rev-parse", "HEAD"]).strip()
        dirty = self.shell(node, ["git", "-C", node["source"], "status", "--porcelain"]).strip()
        paths = lib.wrapper_paths(self.shell(node, ["cat", node["runner"]]))
        if not paths["build_info"] or not paths["runner"]:
            raise RuntimeError(f"[{node['name']}] {node['runner']} is not a cas-two-host wrapper")
        build = json.loads(self.shell(node, ["cat", paths["build_info"]]))
        binaries = {k: build[k] for k in ("host", "fleet", "qemu")}
        if node.get("native"):
            native = lib.wrapper_paths(self.shell(node, ["cat", node["native"]]))
            native_build = json.loads(self.shell(node, ["cat", native["build_info"]]))
            binaries["native_host"] = native_build["host"]
            binaries["native_daemon"] = native_build["daemon"]
            binaries["native_harness"] = node["native"]
        hashes = {}
        for line in self.shell(node, ["sha256sum", *binaries.values()]).splitlines():
            digest, path = line.split(maxsplit=1)
            hashes[path] = digest
        return {
            "revision": revision,
            "dirty": dirty,
            "build_revision": build.get("source_revision"),
            "build_info": paths["build_info"],
            "binaries": {name: hashes[path] for name, path in binaries.items()},
            "binary_paths": binaries,
            "runner_support": lib.runner_supports(self.shell(node, ["cat", paths["runner"]])),
        }

    def manifest(self):
        """Record revision and binary hashes once; a resumed campaign must
        still see the same revision and binaries on every node."""
        identities = {node["name"]: self.node_identity(node) for node in self.nodes}
        first = next(iter(identities.values()))
        for name, identity in identities.items():
            if identity["revision"] != first["revision"] or identity["binaries"] != first["binaries"]:
                raise RuntimeError(f"node {name} differs from the first node in revision or binaries")
        fresh = {
            "campaign": self.name,
            "revision": first["revision"],
            "build_revision": first["build_revision"],
            "binaries": first["binaries"],
            "runner_support": first["runner_support"],
            "nodes": identities,
            "arms": {name: arm["_sha256"] for name, arm in self.arms.items()},
            "plan": {"path": str(self.plan_path), "sha256": sha256_file(self.plan_path) if self.plan_path else None},
            "nodes_config_sha256": sha256_file(self.nodes_path),
            "driver": {p.name: sha256_file(p) for p in (HERE / "campaign.py", HERE / "campaignlib.py", HERE / "setup.sh", HERE / "r0.sh", HERE / "pingpong.py")},
            "loopback": self.loopback,
            "evidence_label": "development" if self.loopback else "native",
        }
        if self.manifest_path.exists():
            old = json.loads(self.manifest_path.read_text())
            for key in ("revision", "binaries"):
                if old[key] != fresh[key]:
                    raise RuntimeError(f"campaign {self.name} is bound to {key} {old[key]!r}; nodes now have {fresh[key]!r}")
            old.update({k: v for k, v in fresh.items() if k not in ("created", "fills")})
            fresh = old
        else:
            fresh["created"] = now()
            fresh["fills"] = {}
        write_json(self.manifest_path, fresh)
        self.manifest_data = fresh
        return fresh

    # ------------------------------------------------------------ fills

    def race_budget(self, allow_pending):
        fills = self.manifest_data["fills"]
        if "race_budget" in fills:
            return fills["race_budget"]["us"]
        explicit = self.nodes_config.get("race_budget_us")
        if explicit is not None:
            fills["race_budget"] = {"us": int(explicit), "source": "nodes config race_budget_us"}
        else:
            sources, budgets = [], []
            for path in sorted(self.units_dir.glob("d*-frozen-reads-memory-r*/controller/results.json")):
                try:
                    us, p50s = lib.race_budget_us(json.loads(path.read_text()))
                except (ValueError, KeyError):
                    continue
                sources.append({"results": str(path), "p50_ns": p50s, "us": us})
                budgets.append(us)
            if not budgets:
                if allow_pending:
                    return None
                raise Blocked("RACE_BUDGET_US needs a successful frozen reads-memory unit or nodes config race_budget_us")
            import statistics

            fills["race_budget"] = {"us": round(statistics.median(budgets)), "source": sources, "rule": "median over frozen reads-memory units of the quiet random QD1 clat p50"}
        write_json(self.manifest_path, self.manifest_data)
        return fills["race_budget"]["us"]

    def working_set(self):
        fills = self.manifest_data["fills"]
        if "working_set" not in fills:
            bundle = self.nodes_config.get("bundle_actions")
            if not bundle:
                raise Blocked("WORKING_SET_FROM_BUNDLE needs nodes config bundle_actions")
            fills["working_set"] = lib.working_set_from_bundle(bundle)
            write_json(self.manifest_path, self.manifest_data)
        return fills["working_set"]["commands"]

    def fills(self, arm, allow_pending=False):
        needed = set(lib.unresolved(arm.get("config", {})))
        fills = {}
        if "RACE_BUDGET_US" in needed:
            fills["RACE_BUDGET_US"] = self.race_budget(allow_pending)
        if "WORKING_SET_FROM_BUNDLE" in needed:
            fills["WORKING_SET_FROM_BUNDLE"] = self.working_set()
        return fills

    # ------------------------------------------------------------ preflight

    def preflight(self, inventories):
        """Problems per unit, without launching anything. A unit with
        problems is blocked; the others still run."""
        report = {}
        for day, unit, rep in self.schedule:
            uid = lib.unit_id(day, unit, rep)
            arm = self.arms[unit.arm]
            problems = self.check_pinning(arm, unit.case, inventories)
            try:
                checker = getattr(self, f"check_{arm['runner'].replace('-', '_')}")
                problems += checker(arm, unit.case)
            except Blocked as error:
                problems.append(str(error))
            report[uid] = problems
        return report

    def check_pinning(self, arm, case, inventories):
        problems = []
        guests = lib.case_guests(arm, case)
        cores = lib.guest_cores(arm, case)
        for index, node in enumerate(self.nodes):
            sets = arm["pinning"].get(node["node_type"])
            if sets is None:
                problems.append(f"{node['name']}: arm has no pinning for node type {node['node_type']!r}")
                continue
            ncpu = inventories[node["name"]]["hardware"]["logical_cpus"]
            problems += [f"{node['name']}: {p}" for p in lib.check_pinning(sets, index, ncpu, cores, guests)]
            if arm["runner"] != "two-host":
                break
        return problems

    def check_two_host(self, arm, case):
        problems = []
        for key in arm.get("requires_runner_settings", []):
            if not self.manifest_data["runner_support"].get(key):
                problems.append(f"built runner does not know settings.{key}; the arm would be mislabelled")
        for node in self.nodes:
            if self.mount_type(node)["fstype"] != "xfs":
                problems.append(f"{node['name']}: {node['storage_mount']} is not a mounted XFS filesystem")
            if not self.exists(node, node["runner"], "-x"):
                problems.append(f"{node['name']}: runner {node['runner']} missing")
            if case == "zfs-incremental" and not node.get("zfs_parent"):
                problems.append(f"{node['name']}: zfs-incremental needs zfs_parent")
        try:
            self.fills(arm, allow_pending=True)
        except (Blocked, OSError, ValueError) as error:
            problems.append(str(error))
        return problems

    def check_native(self, arm, case):
        problems = []
        entry = arm["cases"][case]
        node = self.nodes[0]
        if entry["guests"] > lib.NATIVE_MAX_GUESTS:
            problems.append(f"cas-native-run boots at most {lib.NATIVE_MAX_GUESTS} guests; {entry['guests']} needs a harness extension")
        if arm.get("filesystem", "xfs") != "xfs":
            problems.append(f"cas-native-run requires an XFS storage filesystem; the {arm['filesystem']} arm needs its own runner")
        if not node.get("native"):
            problems.append("node 0 has no native runner path")
        elif not self.exists(node, node["native"], "-x"):
            problems.append(f"native runner {node['native']} missing")
        if self.mount_type(node)["fstype"] != "xfs":
            problems.append(f"{node['storage_mount']} is not a mounted XFS filesystem")
        if not self.loopback and not (node.get("device") and self.exists(node, node["device"], "-b")):
            problems.append("node 0 device missing or not a block device")
        if not self.exists(node, f"{node['source']}/experiments/native/workload.sh"):
            problems.append("experiments/native/workload.sh missing on node 0")
        return problems

    def check_r0(self, arm, case):
        problems = []
        for node in self.nodes[:2]:
            for name in ("r0.sh", "pingpong.py"):
                if not self.exists(node, self.script(node, name)):
                    problems.append(f"{node['name']}: {name} missing")
            if self.mount_type(node)["fstype"] != "xfs":
                problems.append(f"{node['name']}: {node['storage_mount']} is not a mounted XFS filesystem")
            out = self.shell(node, ["bash", self.script(node, "r0.sh"), "--check", "--source", node["source"]], check=False)
            if "fio:" not in out:
                problems.append(f"{node['name']}: r0.sh --check found no fio")
        return problems

    # ------------------------------------------------------------ units

    def run_unit(self, day, unit, rep, uid):
        arm = self.arms[unit.arm]
        unit_dir = self.units_dir / uid
        unit_dir.mkdir(parents=True)
        record = {"unit": uid, "day": day, "arm": unit.arm, "case": unit.case, "repetition": rep, "runner": arm["runner"], "started": now(), "evidence_label": self.manifest_data["evidence_label"]}
        write_json(unit_dir / "unit.json", record)
        start = time.monotonic()
        try:
            runner = getattr(self, f"run_{arm['runner'].replace('-', '_')}")
            record["command"] = runner(arm, unit.case, uid, unit_dir)
            record["success"] = lib.unit_state(unit_dir) == "done"
        except Blocked as error:
            record.update(success=False, blocked=str(error))
        except (RuntimeError, subprocess.SubprocessError, OSError, ValueError) as error:
            record.update(success=False, error=repr(error))
        record.update(finished=now(), seconds=round(time.monotonic() - start, 1))
        write_json(unit_dir / "unit.json", record)
        return record

    def run_two_host(self, arm, case, uid, unit_dir):
        config = lib.two_host_config(arm, self.nodes, self.name, uid, case, self.fills(arm))
        left = lib.unresolved(config)
        if left:
            raise Blocked(f"unresolved placeholders {left}")
        config_path = unit_dir / "config.json"
        write_json(config_path, config)
        command = [self.nodes[0]["runner"], "--config", str(config_path), "--output", str(unit_dir / "controller")]
        self.launch(command, unit_dir / "runner.log")
        for node in self.nodes[1:]:
            self.pull(node, lib.host_output(node, self.name, uid), unit_dir / f"host-{node['name']}")
        return command

    def run_native(self, arm, case, uid, unit_dir):
        entry = arm["cases"][case]
        node = self.nodes[0]
        if entry["guests"] > lib.NATIVE_MAX_GUESTS:
            raise Blocked(f"cas-native-run boots at most {lib.NATIVE_MAX_GUESTS} guests")
        if arm.get("filesystem", "xfs") != "xfs":
            raise Blocked(f"cas-native-run requires XFS; {arm['filesystem']} arm has no runner")
        sets = arm["pinning"][node["node_type"]]
        cpus = lib.format_cpuset(lib.parse_cpuset(lib.pin_for(sets, "daemon", 0)) + lib.parse_cpuset(lib.pin_for(sets, "guests", 0)))
        command = [
            "taskset", "-c", cpus, node["native"], "--backend", arm["backend"], "--guests", str(entry["guests"]),
            "--device", node.get("device") or self.mount_type(node)["source"],
            "--storage", f"{node['storage_mount']}/{self.name}/{uid}", "--output", str(unit_dir / "native"),
            "--script", f"{node['source']}/experiments/native/workload.sh",
        ]
        for value in entry.get("script_args", []):
            command += ["--script-arg", value]
        if self.loopback:
            command.append("--allow-loop-device")
        self.launch(command, unit_dir / "runner.log")
        return command

    def run_r0(self, arm, case, uid, unit_dir):
        a, b = self.nodes[:2]
        sets = arm["pinning"]
        peer = f"{b['address']}:{R0_PORT}"
        label = self.manifest_data["evidence_label"]
        common = ["--seconds", str(arm.get("seconds", 30)), "--label", label]
        b_out = f"{b['output_root']}/{self.name}/units/{uid}/node-{b['name']}"
        b_cmd = ["bash", self.script(b, "r0.sh"), "--mount", b["storage_mount"], "--output", b_out, "--source", b["source"], "--cpus", lib.pin_for(sets[b["node_type"]], "controller", 1), "--role", "server", "--peer", peer, *common]
        a_cmd = ["bash", self.script(a, "r0.sh"), "--mount", a["storage_mount"], "--output", str(unit_dir / f"node-{a['name']}"), "--source", a["source"], "--cpus", lib.pin_for(sets[a["node_type"]], "controller", 0), "--role", "client", "--peer", peer, *common]
        with (unit_dir / f"server-{b['name']}.log").open("w") as log:
            server = subprocess.Popen(self.wrap(b, b_cmd), stdout=log, stderr=subprocess.STDOUT)
            try:
                self.launch(a_cmd, unit_dir / "runner.log")
            finally:
                try:
                    server.wait(timeout=600)
                except subprocess.TimeoutExpired:
                    server.kill()
        self.pull(b, b_out, unit_dir / f"node-{b['name']}")
        nodes = {}
        for node in (a, b):
            path = unit_dir / f"node-{node['name']}" / "r0.json"
            nodes[node["name"]] = json.loads(path.read_text()) if path.exists() else {"success": False, "error": "no r0.json"}
        write_json(unit_dir / "r0.json", {"success": all(n.get("success") for n in nodes.values()) and server.returncode == 0, "server_rc": server.returncode, "nodes": nodes, "evidence_label": label})
        return {"a": a_cmd, "b": b_cmd}

    def launch(self, command, log_path):
        write_json(log_path.with_suffix(".command.json"), {"argv": command, "cwd": os.getcwd(), "started": now()})
        self.log("launch " + shlex.join(command)[:200])
        with log_path.open("a") as log:
            result = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, timeout=UNIT_TIMEOUT)
        self.log(f"exit {result.returncode} {command[0]}")
        return result.returncode

    # ------------------------------------------------------------ export

    def export(self, uid=None):
        """SHA256SUMS over the unit (or campaign-level files), then rsync to
        the export destination, local path or user@host:path."""
        target = self.units_dir / uid if uid else self.dir
        sums = []
        for path in sorted(target.rglob("*")):
            if not path.is_file() or path.name == "SHA256SUMS":
                continue
            if uid is None and path.relative_to(target).parts[0] == "units":
                continue
            sums.append(f"{sha256_file(path)}  {path.relative_to(target)}")
        (target / "SHA256SUMS").write_text("\n".join(sums) + "\n")
        if not self.export_path:
            self.set_status(last_export="disabled: no export path")
            return
        dest = f"{self.export_path.rstrip('/')}/{self.name}/" + (f"units/{uid}/" if uid else "")
        argv = ["rsync", "-a", *([] if uid else ["--exclude", "/units/"]), f"{target}/", dest]
        remote = ":" in dest and not dest.startswith("/")
        if remote:
            argv[1:1] = ["--rsync-path", f"mkdir -p {shlex.quote(dest.split(':', 1)[1])} && rsync"]
        else:
            Path(dest).mkdir(parents=True, exist_ok=True)
        if not remote and not shutil.which("rsync"):
            shutil.copytree(target, dest, dirs_exist_ok=True, ignore=None if uid else shutil.ignore_patterns("units"))
            self.set_status(last_export=f"{now()} copytree {dest}", export=self.export_path)
            return
        result = subprocess.run(argv, capture_output=True, text=True, timeout=3600)
        self.log(f"export rc={result.returncode} {dest} {result.stderr.strip()[-200:]}")
        self.set_status(last_export=f"{now()} rc={result.returncode} {dest}", export=self.export_path)

    # ------------------------------------------------------------ phases

    def run(self):
        self.guard()
        self.set_status(campaign=self.name, host=socket.gethostname(), pid=os.getpid(), launched=self.status.get("launched") or now(), phase="inventory-start", plan=str(self.plan_path), export=self.export_path, dry_run=self.args.dry_run)
        inventories = self.inventories("start")
        self.set_status(phase="manifest")
        manifest = self.manifest()
        self.set_status(revision=manifest["revision"], phase="preflight")
        report = self.preflight(inventories)
        write_json(self.dir / ("dry-run.json" if self.args.dry_run else "preflight.json"), {"campaign": self.name, "time": now(), "revision": manifest["revision"], "evidence_label": manifest["evidence_label"], "units": report, "schedule": [[d, u.arm, u.case, r] for d, u, r in self.schedule]})
        for uid, problems in report.items():
            self.unit_status(uid, state="blocked" if problems else lib.unit_state(self.units_dir / uid).replace("absent", "pending"), note="; ".join(problems))
        if self.args.dry_run:
            self.set_status(phase="dry-run-finished", current=None)
            self.log(f"dry run: {sum(1 for p in report.values() if not p)} runnable, {sum(1 for p in report.values() if p)} blocked of {len(report)} units")
            return all(not p for p in report.values())
        self.export()
        self.set_status(phase="units")
        for day, unit, rep in self.schedule:
            uid = lib.unit_id(day, unit, rep)
            if report[uid]:
                continue
            self.step(day, unit, rep, uid)
        self.set_status(phase="inventory-end", current=None)
        self.inventories("end")
        self.export()
        self.set_status(phase="finished")
        return True

    def step(self, day, unit, rep, uid):
        unit_dir = self.units_dir / uid
        state = lib.unit_state(unit_dir)
        decision = lib.decide(state, self.args.redo)
        if decision == "skip":
            self.unit_status(uid, state="done", note="kept from an earlier run")
            return
        if decision == "refuse":
            self.unit_status(uid, state="refused", note=f"outputs exist in state {state}; rerun with --redo")
            self.log(f"refuse {uid}: {state}")
            return
        if state != "absent":
            attempts = len(list(self.units_dir.glob(f"{uid}.attempt-*")))
            unit_dir.rename(self.units_dir / f"{uid}.attempt-{attempts + 1}")
        self.set_status(current=uid)
        self.unit_status(uid, state="running", started=now())
        record = self.run_unit(day, unit, rep, uid)
        state = "done" if record.get("success") else "blocked" if record.get("blocked") else "failed"
        self.unit_status(uid, state=state, seconds=record.get("seconds"), error=record.get("error") or record.get("blocked"))
        self.export(uid)
        self.set_status(current=None)

    def guard(self):
        pid = self.status.get("pid")
        if pid and pid != os.getpid() and Path(f"/proc/{pid}").exists() and self.status.get("phase") not in (None, "finished", "dry-run-finished"):
            raise SystemExit(f"campaign {self.name} already running as pid {pid} (phase {self.status.get('phase')})")

    def start(self):
        """Detach the worker so a lost SSH session does not end the campaign."""
        self.dir.mkdir(parents=True, exist_ok=True)
        worker = [sys.executable, str(HERE / "campaign.py"), "run", "--nodes", str(self.nodes_path), "--plan", str(self.plan_path), "--arms", str(self.args.arms)]
        if self.export_path:
            worker += ["--export", self.export_path]
        if self.args.redo:
            worker.append("--redo")
        if self.args.dry_run:
            worker.append("--dry-run")
        unit = f"cas-campaign-{self.name}"
        if shutil.which("systemd-run"):
            scope = [] if os.geteuid() == 0 else ["--user"]
            # The unit gets this shell's PATH and HOME, so the Nix profile
            # (fio through nix develop, the runner wrappers) stays reachable.
            env = [f"--setenv={k}={os.environ[k]}" for k in ("PATH", "HOME", "NIX_PATH", "SSH_AUTH_SOCK") if k in os.environ]
            argv = ["systemd-run", *scope, "--unit", unit, "--collect", "--same-dir", *env, "-p", f"StandardOutput=append:{self.dir}/driver.out", "-p", f"StandardError=append:{self.dir}/driver.out", "--", *worker]
            result = subprocess.run(argv, capture_output=True, text=True)
            if result.returncode == 0:
                self.set_status(launched=now(), launched_via=f"systemd-run unit {unit}")
                print(f"started {unit} via systemd-run; status: campaign.py status --nodes {self.nodes_path}")
                return
            print(f"systemd-run failed ({result.stderr.strip()[-200:]}); falling back to nohup", file=sys.stderr)
        with (self.dir / "driver.out").open("a") as out:
            process = subprocess.Popen(["nohup", *worker], stdout=out, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True)
        self.set_status(launched=now(), launched_via=f"nohup setsid pid {process.pid}")
        print(f"started pid {process.pid} via nohup; status: campaign.py status --nodes {self.nodes_path}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=["start", "run", "status", "dry-run"])
    parser.add_argument("--nodes", required=True, help="campaign nodes config (see cloudlab.example.json)")
    parser.add_argument("--plan", help="day plan JSON")
    parser.add_argument("--arms", default=str(lib.ARMS_DIR))
    parser.add_argument("--export", help="rsync destination for evidence; overrides the nodes config")
    parser.add_argument("--redo", action="store_true", help="rerun units whose outputs exist (old outputs are kept as .attempt-N)")
    parser.add_argument("--dry-run", action="store_true", help="validate configs, pinning, binaries and inputs; launch nothing")
    args = parser.parse_args()
    if args.command == "dry-run":
        args.command, args.dry_run = "run", True
    if args.command in ("start", "run") and not args.plan:
        parser.error("--plan is required")
    campaign = Campaign(args)
    if args.command == "status":
        if not campaign.status:
            print(f"no status for campaign {campaign.name} under {campaign.dir}")
            return 1
        print("\n".join(lib.status_lines(campaign.status)))
        return 0
    if args.command == "start":
        campaign.start()
        return 0
    return 0 if campaign.run() else 2


if __name__ == "__main__":
    sys.exit(main())
