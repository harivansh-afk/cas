"""Two-host guest/fleet experiment. See docs/two-host-experiments.md."""

import argparse
import itertools
import json
import os
import queue
import re
import shlex
import signal
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from agent import CLONE, IMAGE, Agent, stop, wait_for, write_json

DEFAULTS = {
    "image_bytes": 512 * 1024**2,
    "data_bytes": 8 * 1024**2,
    "segment_bytes": 8 * 1024**2,
    "staging_bytes": 1024**3,
    "cache_bytes": 64 * 1024**2,
    "ship_rate_bytes": 16 * 1024**2,
    "seconds": 30,
    "repeats": 5,
    "depths": [1, 4, 16, 64],
    "inline_us": 50,
    # Measurement arms of the read path; both off is the frozen behaviour.
    "peer_race_us": None,
    "fast_settled_reads": False,
}


def rsync_bytes(text):
    counts = []
    for direction in ("sent", "received"):
        match = re.search(
            r"^Total bytes " + direction + r": ([\d,]+)$", text, re.MULTILINE
        )
        if not match:
            raise ValueError("rsync --stats lacks exact byte counters")
        counts.append(int(match[1].replace(",", "")))
    return sum(counts)


def deltas(before, after):
    """Sent on each end once; never add sent and received on both ends."""
    flows = ("ship", "sync", "provision", "migration", "collection", "control")
    result = {
        flow: sum(
            b["fleet"][flow]["sent"] - a["fleet"][flow]["sent"]
            for a, b in zip(before, after, strict=True)
        )
        for flow in flows
    }
    if any(value < 0 for value in result.values()):
        raise ValueError("counter reset inside a measurement")
    return result


def peer_delta(before, after):
    return {
        key: after["peer"]["counters"][key] - before["peer"]["counters"][key]
        for key in ("gets", "verified", "errors", "server_memory", "server_store")
    }


def classify(before, after, target):
    counters = peer_delta(before, after)
    if counters["errors"]:
        raise ValueError("remote read error in cohort")
    if target == "memory" and (
        counters["server_store"] or not counters["server_memory"]
    ):
        raise ValueError(f"peer-memory cohort did not stay in memory: {counters}")
    if target == "storage" and (
        counters["server_memory"] or not counters["server_store"]
    ):
        raise ValueError(f"peer-storage cohort did not bypass chunk cache: {counters}")
    return counters


def has_overlap(samples):
    return any(s["ship_chunks_acked"] > 0 and s["verified_gets"] > 0 for s in samples)


def phases(report, since):
    rows = []
    for record in report["peer"]["recent"]:
        if record["received_ns"] < since:
            continue
        if not record["success"]:
            raise ValueError("failed read in timing ring")
        times = [
            record["received_ns"],
            record["phases"]["send_ns"],
            record["phases"]["first_byte_ns"],
            record["phases"]["verified_ns"],
            record["completed_ns"],
        ]
        if sorted(times) != times:
            raise ValueError("nonmonotonic read milestones")
        rows.append(
            {
                "id": record["id"],
                "chunks": record["chunks"],
                "ns": [b - a for a, b in itertools.pairwise(times)],
                "total_ns": times[-1] - times[0],
            }
        )
    return rows


class Remote:
    def __init__(self, node, build_info, directory):
        if node.get("ssh"):
            command = [
                "ssh",
                "-T",
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=yes",
                node["ssh"],
                shlex.join([node["runner"], "--agent"]),
            ]
        else:
            command = [sys.executable, __file__, "--build-info", build_info, "--agent"]
        self.log = (directory / "agent.log").open("x")
        self.process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.log,
            text=True,
            start_new_session=True,
        )
        self.replies = queue.Queue()

        def read():
            try:
                for line in self.process.stdout:
                    self.replies.put(json.loads(line))
            except (OSError, ValueError) as error:
                self.replies.put({"error": repr(error)})
            finally:
                self.replies.put({"error": "agent connection closed"})

        self.reader = threading.Thread(target=read, daemon=True)
        self.reader.start()
        self.events = (directory / "rpc.jsonl").open("x")

    def call(self, method, **args):
        request = {"method": method, "args": args}
        self.process.stdin.write(json.dumps(request) + "\n")
        self.process.stdin.flush()
        value = self.replies.get(timeout=2400 if method == "boot" else 900)
        self.events.write(json.dumps({"request": request, "response": value}) + "\n")
        self.events.flush()
        if "error" in value:
            raise RuntimeError(f"agent {method}: {value['error']}")
        return value["result"]

    def close(self):
        try:
            self.process.stdin.close()
        except BrokenPipeError:
            pass
        try:
            self.process.wait(timeout=30)
        except subprocess.TimeoutExpired:
            stop(self.process)
        self.reader.join(timeout=1)
        self.events.close()
        self.log.close()


def parallel(items, fn):
    with ThreadPoolExecutor(max_workers=16) as executor:
        return list(executor.map(fn, items))


class Experiment:
    def __init__(self, config, build_info, output):
        self.config = config
        self.settings = {**DEFAULTS, **config.get("settings", {})}
        if not 2 <= len(config["nodes"]) <= 16:
            raise ValueError("runner supports 2 through 16 nodes")
        if not (0 < self.settings["data_bytes"] < self.settings["image_bytes"] // 2):
            raise ValueError(
                "data must fit the guest image with filesystem and drift headroom"
            )
        if self.settings["cache_bytes"] < self.settings["data_bytes"] * 2:
            raise ValueError("memory cohort needs cache headroom for data and metadata")
        if (
            not 1 <= self.settings["seconds"] <= 300
            or not 1 <= self.settings["repeats"] <= 20
        ):
            raise ValueError("invalid run duration/repeat count")
        self.output = output
        self.agents = []
        for index, node in enumerate(config["nodes"]):
            directory = output / str(index)
            directory.mkdir()
            self.agents.append(Remote(node, build_info, directory))
        self.results = {
            "schema_version": 1,
            "config": config,
            "settings": self.settings,
            "started_unix_ns": time.time_ns(),
            "paper_result": False,
            "cases": {},
        }

    def save(self):
        write_json(self.output / "results.json", self.results)

    def report(self):
        return parallel(self.agents, lambda agent: agent.call("report"))

    def fio(self, node, guest, kind, name, **kwargs):
        agent = self.agents[node]
        agent.call("fio", guest=guest, kind=kind, name=name, **kwargs)
        return agent.call("finish_fio", name=name)

    def fleet(self, operation, **kwargs):
        argv = [operation]
        for key, value in kwargs.items():
            for item in value if isinstance(value, list) else [value]:
                argv += ["--" + key, str(item)]
        start = time.monotonic_ns()
        result = self.agents[0].call("fleet", argv=argv)
        result["controller_elapsed_ns"] = time.monotonic_ns() - start
        return result

    def sync(self):
        return [
            self.fleet("sync", address=node["bulk"]) for node in self.config["nodes"]
        ]

    def start(self, name, k=None, cache=0, force=False):
        works = parallel(self.agents, lambda agent: agent.call("start_case", name=name))
        if k:
            parallel(
                range(len(self.agents)), lambda n: self.agents[n].call("init", node=n)
            )
            parallel(
                range(len(self.agents)),
                lambda n: self.agents[n].call(
                    "host",
                    node=n,
                    k=k,
                    members=self.config["nodes"],
                    cache=cache if n == 0 else 0,
                    force=force and n == 1,
                    images=None if n < 2 else [],
                ),
            )

            def ready():
                try:
                    return self.report()
                except RuntimeError as error:
                    if "cas-host exited" in str(error):
                        raise
                    return False

            wait_for(ready, 120)
        return works

    def end(self):
        parallel(self.agents, lambda agent: agent.call("end_case"))
        self.save()

    def seed(self, backend="cas"):
        a = self.agents[0]
        a.call("boot", name="source", backend=backend, format=True)
        self.fio(0, "source", "seed", "seed")
        digest = a.call(
            "shell",
            guest="source",
            script="cd /mnt/cas; sha256sum data; echo 0 > epoch; sync -f .",
        ).split()[0]
        return digest

    def verify(self, node, guest, digest, continue_work=False):
        script = f'cd /mnt/cas; echo {shlex.quote(digest + "  data")} | sha256sum -c -; test "$(cat epoch)" = 0'
        if continue_work:
            script += '; echo 1 > epoch; sync -f .; test "$(cat epoch)" = 1'
        return self.agents[node].call(
            "shell", guest=guest, script=script + "; sync -f ."
        )

    def provision(self):
        a = self.agents[0]
        a.call("settle")
        self.sync()
        a.call("shell", guest="source", script="fsfreeze -f /mnt/cas")
        before = self.report()
        start = time.monotonic_ns()
        try:
            result = self.fleet(
                "provision",
                source=self.config["nodes"][0]["bulk"],
                destination=self.config["nodes"][1]["bulk"],
                image=IMAGE,
                target=CLONE,
            )
        finally:
            a.call("shell", guest="source", script="fsfreeze -u /mnt/cas")
        return {
            "daemon": result,
            "elapsed_ns": time.monotonic_ns() - start,
            "wire": deltas(before, self.report()),
        }

    def flows(self, k):
        name = f"flows-k{k}"
        self.start(name, k=k)
        result = self.results["cases"][name] = {}
        a, b = self.agents[:2]
        digest = self.seed()
        result["provision"] = self.provision()
        start = time.monotonic_ns()
        b.call("boot", name="clone", image=CLONE)
        self.verify(1, "clone", digest)
        result["provision"]["boot_verify_ns"] = time.monotonic_ns() - start
        parallel(self.agents, lambda agent: agent.call("settle"))
        self.sync()
        before = self.report()
        start = time.monotonic_ns()
        drift = parallel(
            range(2),
            lambda n: self.agents[n].call(
                "drift", guest="source" if n == 0 else "clone"
            ),
        )
        parallel(self.agents, lambda agent: agent.call("settle"))
        result["sync"] = {
            "drift": drift,
            "explicit": self.sync(),
            "elapsed_ns": time.monotonic_ns() - start,
            "wire": deltas(before, self.report()),
            "workload": "identical seeded independent drift file",
        }
        hashes = parallel(
            range(2),
            lambda n: self.agents[n].call(
                "shell",
                guest="source" if n == 0 else "clone",
                script="cd /mnt/cas; sha256sum drift",
            ),
        )
        if hashes[0] != hashes[1]:
            raise ValueError("drift oracle differs")
        result["sweep"] = self.fleet(
            "sweep",
            member=[node["bulk"] for node in self.config["nodes"]],
        )
        result["capacity"] = {
            "hosts": self.report(),
            "files": parallel(self.agents, lambda agent: agent.call("capacity")),
        }
        before = self.report()
        result["cold_read"] = self.fio(0, "source", "verify", "cold-read")
        result["cold_read_peer"] = peer_delta(before[0], self.report()[0])
        b.call("stop_guest", name="clone")
        start = time.monotonic_ns()
        shutdown = a.call("stop_guest", name="source")
        cut = time.monotonic_ns()
        parallel(self.agents, lambda agent: agent.call("restart_host"))

        def recovered():
            try:
                return self.report()
            except RuntimeError:
                return False

        before = wait_for(recovered, 120)
        recovered_at = time.monotonic_ns()
        transfer = self.fleet(
            "migrate",
            source=self.config["nodes"][0]["bulk"],
            destination=self.config["nodes"][1]["bulk"],
            image=IMAGE,
        )
        boot = time.monotonic_ns()
        b.call("boot", name="resumed", image=IMAGE)
        self.verify(1, "resumed", digest, continue_work=True)
        drift_after = b.call(
            "shell", guest="resumed", script="cd /mnt/cas; sha256sum drift"
        )
        if drift_after != hashes[0]:
            raise ValueError("migration lost acknowledged drift data")
        end = time.monotonic_ns()
        result["migration"] = {
            "downtime_ns": end - start,
            "stop_ns": cut - start,
            "handoff_ns": boot - cut,
            "cold_recovery_ns": recovered_at - cut,
            "boot_verify_continue_ns": end - boot,
            "shutdown": shutdown,
            "daemon": transfer,
            "wire": deltas(before, self.report()),
            "verified_sha256": digest,
            "continued_epoch": 1,
        }
        b.call("stop_guest", name="resumed")
        self.end()

    def observed_read(self, kind, name, depth=1):
        before = self.report()
        last = before
        samples = []
        self.agents[1].call("fio", guest="reader", kind=kind, name=name, depth=depth)
        started = time.monotonic()
        while True:
            if time.monotonic() - started > self.settings["seconds"] + 180:
                raise TimeoutError("fio observation deadline")
            time.sleep(min(0.5, self.settings["seconds"] / 4))
            current = self.report()
            samples.append(
                {
                    "elapsed_ns": int((time.monotonic() - started) * 1e9),
                    "ship_bytes": deltas(last, current)["ship"],
                    "ship_chunks_acked": sum(
                        b["fleet"]["ship"]["put_acked_chunks"]
                        - a["fleet"]["ship"]["put_acked_chunks"]
                        for a, b in zip(last, current, strict=True)
                    ),
                    "verified_gets": peer_delta(last[1], current[1])["verified"],
                }
            )
            last = current
            if not self.agents[1].call("fio_running", name=name):
                break
        fio = self.agents[1].call("finish_fio", name=name)
        return fio, before, self.report(), samples

    def reads(self, target):
        name = "reads-" + target
        self.start(
            name,
            k=len(self.agents),
            cache=self.settings["cache_bytes"] if target == "memory" else 0,
            force=True,
        )
        result = self.results["cases"][name] = {"cohorts": []}
        a, b = self.agents[:2]
        digest = self.seed()
        result["provision"] = self.provision()
        b.call("boot", name="reader", image=CLONE)
        self.verify(1, "reader", digest)
        # Read every block to warm the owner; B never fills its cache.
        self.fio(1, "reader", "verify", "warm")
        for repeat in range(self.settings["repeats"]):
            for depth in self.settings["depths"]:
                for kind in ["random", "sequential"] if depth == 1 else ["sequential"]:
                    tag = f"r{repeat}-{kind}-qd{depth}"
                    fio, before, after, samples = self.observed_read(kind, tag, depth)
                    counters = classify(before[0], after[0], target)
                    if (
                        after[1]["peer"]["counters"]["verified"]
                        <= before[1]["peer"]["counters"]["verified"]
                    ):
                        raise ValueError("no verified remote reads")
                    since = max(
                        (r["completed_ns"] for r in before[1]["peer"]["recent"]),
                        default=0,
                    )
                    result["cohorts"].append(
                        {
                            "name": tag,
                            "load": "quiet",
                            "fio": fio,
                            "samples": samples,
                            "owner_counters": counters,
                            "before": before,
                            "after": after,
                            "recent_phases": phases(after[1], since),
                            "loadavg": os.getloadavg(),
                        }
                    )
                    self.save()
            # GET and shipping travel B->A and A->B respectively, on separate sockets.
            tag = f"r{repeat}-contention"
            a.call(
                "fio",
                guest="source",
                kind="ship",
                name=tag + "-writer",
                seconds=self.settings["seconds"] + 10,
                depth=4,
            )
            fio, before, after, samples = self.observed_read("random", tag)
            if not has_overlap(samples):
                raise ValueError(
                    "contention run has no observed overlap of acknowledged PUT chunks and verified GETs"
                )
            writer = a.call("finish_fio", name=tag + "-writer")
            since = max(
                (r["completed_ns"] for r in before[1]["peer"]["recent"]), default=0
            )
            result["cohorts"].append(
                {
                    "name": tag,
                    "load": "compaction-shipping",
                    "fio": fio,
                    "writer": writer,
                    "samples": samples,
                    "wire": deltas(before, after),
                    "owner_counters": peer_delta(before[0], after[0]),
                    "recent_phases": phases(after[1], since),
                    "before": before,
                    "after": after,
                }
            )
            a.call("settle")
            self.sync()
            self.fleet(
                "sweep",
                member=[node["bulk"] for node in self.config["nodes"]],
            )
            self.fio(1, "reader", "verify", tag + "-rewarm")
        b.call("stop_guest", name="reader")
        a.call("stop_guest", name="source")
        self.end()

    def independent(self):
        self.start("independent-stores")
        parallel(self.agents, lambda agent: agent.call("init", node=0))
        parallel(
            range(len(self.agents)),
            lambda n: self.agents[n].call(
                "host", node=n, k=0, members=self.config["nodes"], cache=0
            ),
        )
        parallel(
            self.agents, lambda agent: agent.call("boot", name="guest", format=True)
        )
        parallel(
            range(len(self.agents)), lambda n: self.fio(n, "guest", "seed", "seed")
        )
        parallel(self.agents, lambda agent: agent.call("drift", guest="guest"))
        snapshots = parallel(self.agents, lambda agent: agent.call("settle"))
        self.results["cases"]["independent-stores"] = {
            "telemetry": snapshots,
            "capacity": parallel(self.agents, lambda agent: agent.call("capacity")),
            "comparison": "N independent CAS stores, same seeded files, independent ext4 format metadata; not ZFS",
        }
        parallel(self.agents, lambda agent: agent.call("stop_guest", name="guest"))
        self.end()

    def baseline(self):
        works = self.start("raw-rsync")
        a, b = self.agents[:2]
        result = self.results["cases"]["raw-rsync"] = {"cohorts": []}
        digest = self.seed("raw")
        for repeat in range(self.settings["repeats"]):
            for depth in self.settings["depths"]:
                for kind in ["random", "sequential"] if depth == 1 else ["sequential"]:
                    tag = f"r{repeat}-{kind}-qd{depth}"
                    result["cohorts"].append(
                        {
                            "name": tag,
                            "fio": self.fio(0, "source", kind, tag, depth=depth),
                        }
                    )
        provision_start = time.monotonic_ns()
        a.call("stop_guest", name="source")
        dest = {
            **self.config["nodes"][1],
            "work": works[1],
            "rsync": self.results["hosts"][1]["build"]["rsync"],
        }
        if not dest.get("ssh"):
            dest["url"] = b.call("raw_receiver")
        result["provision"] = a.call("rsync", destination=dest, target=CLONE)
        b.call("boot", name="clone", image=CLONE, backend="raw")
        self.verify(1, "clone", digest)
        result["provision"]["shutdown_copy_boot_verify_ns"] = (
            time.monotonic_ns() - provision_start
        )
        # Restart A to give the raw arm the same running-guest starting condition.
        a.call("boot", name="drift-source", backend="raw")
        start = time.monotonic_ns()
        result["drift"] = parallel(
            range(2),
            lambda n: self.agents[n].call(
                "drift", guest="drift-source" if n == 0 else "clone"
            ),
        )
        result["drift_ns"] = time.monotonic_ns() - start
        hashes = parallel(
            range(2),
            lambda n: self.agents[n].call(
                "shell",
                guest="drift-source" if n == 0 else "clone",
                script="cd /mnt/cas; sha256sum drift",
            ),
        )
        if hashes[0] != hashes[1]:
            raise ValueError("raw drift oracle differs")
        b.call("stop_guest", name="clone")
        start = time.monotonic_ns()
        shutdown = a.call("stop_guest", name="drift-source")
        cut = time.monotonic_ns()
        transfer = a.call("rsync", destination=dest, target=IMAGE)
        boot = time.monotonic_ns()
        b.call("boot", name="resumed", backend="raw")
        self.verify(1, "resumed", digest, continue_work=True)
        if (
            b.call("shell", guest="resumed", script="cd /mnt/cas; sha256sum drift")
            != hashes[0]
        ):
            raise ValueError("rsync migration lost drift data")
        end = time.monotonic_ns()
        result["migration"] = {
            "downtime_ns": end - start,
            "stop_ns": cut - start,
            "copy_ns": boot - cut,
            "boot_verify_continue_ns": end - boot,
            "shutdown": shutdown,
            "rsync": transfer,
            "verified_sha256": digest,
            "continued_epoch": 1,
        }
        b.call("stop_guest", name="resumed")
        # Explicit full-image resynchronization control; --ignore-times prevents quick-check skipping.
        result["sync_full_image"] = a.call("rsync", destination=dest, target=CLONE)
        start = time.monotonic_ns()
        b.call("boot", name="resynced", image=CLONE, backend="raw")
        self.verify(1, "resynced", digest)
        if (
            b.call("shell", guest="resynced", script="cd /mnt/cas; sha256sum drift")
            != hashes[0]
        ):
            raise ValueError("rsync synchronization lost drift data")
        result["sync_full_image"]["boot_verify_ns"] = time.monotonic_ns() - start
        b.call("stop_guest", name="resynced")
        result["capacity"] = parallel(self.agents, lambda agent: agent.call("capacity"))
        self.end()

    def zfs_transfer(self, snapshot, base=None):
        a, b = self.agents[:2]
        start = time.monotonic_ns()
        receiver = b.call(
            "zfs_receiver",
            base=base["name"] if base else None,
            guid=base["guid"] if base else None,
        )
        transfer = a.call(
            "zfs_send",
            destination=receiver,
            snapshot=snapshot["name"],
            base=base["name"] if base else None,
        )
        received = b.call("zfs_finish_receive", name=receiver["name"])
        destination = b.call("zfs_snapshot", name=snapshot["name"], create=False)
        if (
            destination["guid"] != snapshot["guid"]
            or transfer["stream_bytes"] != received["bytes"]
        ):
            raise ValueError("ZFS destination snapshot or received bytes differ")
        return {
            "controller_elapsed_ns": time.monotonic_ns() - start,
            "source_snapshot": snapshot,
            "destination_snapshot": destination,
            "common_base": base,
            "transfer": transfer,
            "received": received,
        }

    def zfs_baseline(self):
        self.start("zfs-incremental")
        a, b = self.agents[:2]
        result = self.results["cases"]["zfs-incremental"] = {}
        result["datasets"] = [a.call("zfs_prepare", create=True), b.call("zfs_prepare")]
        digest = self.seed("raw")
        a.call("stop_guest", name="source")
        base = a.call("zfs_snapshot", name="base")
        # The full stream establishes an actual common snapshot, not independently
        # generated files with coincidentally equal bytes. Account for it separately.
        result["common_base_setup"] = self.zfs_transfer(base)
        self.save()
        a.call("boot", name="updated-source", backend="raw")
        a.call("shell", guest="updated-source", script="fsfreeze -f /mnt/cas")
        try:
            provision = a.call("zfs_snapshot", name="provision")
        finally:
            a.call("shell", guest="updated-source", script="fsfreeze -u /mnt/cas")
        result["provision"] = self.zfs_transfer(provision, base)
        start = time.monotonic_ns()
        b.call("boot", name="clone", image=IMAGE, backend="raw")
        self.verify(1, "clone", digest)
        result["provision"]["boot_verify_ns"] = time.monotonic_ns() - start
        start = time.monotonic_ns()
        result["drift"] = parallel(
            range(2),
            lambda n: self.agents[n].call(
                "drift", guest="updated-source" if n == 0 else "clone"
            ),
        )
        result["drift_ns"] = time.monotonic_ns() - start
        hashes = parallel(
            range(2),
            lambda n: self.agents[n].call(
                "shell",
                guest="updated-source" if n == 0 else "clone",
                script="cd /mnt/cas; sha256sum drift",
            ),
        )
        if hashes[0] != hashes[1]:
            raise ValueError("ZFS drift oracle differs")
        b.call("stop_guest", name="clone")
        a.call("shell", guest="updated-source", script="fsfreeze -f /mnt/cas")
        try:
            drift = a.call("zfs_snapshot", name="drift")
        finally:
            a.call("shell", guest="updated-source", script="fsfreeze -u /mnt/cas")
        result["sync"] = self.zfs_transfer(drift, provision)
        b.call("boot", name="synced", image=IMAGE, backend="raw")
        self.verify(1, "synced", digest)
        if (
            b.call("shell", guest="synced", script="cd /mnt/cas; sha256sum drift")
            != hashes[0]
        ):
            raise ValueError("ZFS sync lost drift data")
        b.call("stop_guest", name="synced")
        self.save()
        start = time.monotonic_ns()
        shutdown = a.call("stop_guest", name="updated-source")
        cut = time.monotonic_ns()
        handoff = a.call("zfs_snapshot", name="handoff")
        transfer = self.zfs_transfer(handoff, drift)
        boot = time.monotonic_ns()
        b.call("boot", name="resumed", image=IMAGE, backend="raw")
        self.verify(1, "resumed", digest, continue_work=True)
        if (
            b.call("shell", guest="resumed", script="cd /mnt/cas; sha256sum drift")
            != hashes[0]
        ):
            raise ValueError("ZFS migration lost acknowledged drift data")
        end = time.monotonic_ns()
        result["migration"] = {
            "downtime_ns": end - start,
            "stop_ns": cut - start,
            "copy_ns": boot - cut,
            "boot_verify_continue_ns": end - boot,
            "shutdown": shutdown,
            "zfs": transfer,
            "verified_sha256": digest,
            "continued_epoch": 1,
        }
        b.call("stop_guest", name="resumed")
        result["capacity"] = [a.call("zfs_capacity"), b.call("zfs_capacity")]
        self.end()

    def run(self):
        try:
            identities = parallel(
                range(len(self.agents)),
                lambda n: self.agents[n].call(
                    "configure",
                    node=self.config["nodes"][n],
                    output=self.config["nodes"][n]["output"],
                    settings=self.settings,
                ),
            )
            revisions = [
                identity["build"]["source_revision"] for identity in identities
            ]
            if (
                not revisions[0]
                or any(r != revisions[0] for r in revisions)
                or any(
                    i["build"]["source_path"] != identities[0]["build"]["source_path"]
                    for i in identities
                )
            ):
                raise ValueError(
                    "all hosts must run the same identified source revision"
                )
            self.results.update(source_revision=revisions[0], hosts=identities)
            self.save()
            for case in self.config.get(
                "cases",
                [
                    "flows-k1",
                    "flows-k2",
                    "reads-memory",
                    "reads-storage",
                    "raw-rsync",
                    "zfs-incremental",
                    "independent-stores",
                ],
            ):
                print(f"Running {case}", file=sys.stderr, flush=True)
                if case in ("flows-k1", "flows-k2"):
                    self.flows(int(case[-1]))
                elif case in ("reads-memory", "reads-storage"):
                    self.reads(case.removeprefix("reads-"))
                elif case == "raw-rsync":
                    self.baseline()
                elif case == "zfs-incremental":
                    self.zfs_baseline()
                elif case == "independent-stores":
                    self.independent()
                else:
                    raise ValueError(f"unknown case {case}")
            self.results["success"] = True
        except BaseException as error:
            self.results.update(success=False, error=repr(error))
            raise
        finally:
            self.results["finished_unix_ns"] = time.time_ns()
            self.save()
            for agent in self.agents:
                agent.close()


def serve(build):
    agent = Agent(build)
    try:
        for line in sys.stdin:
            try:
                result = {"result": agent.dispatch(json.loads(line))}
            except Exception as error:  # noqa: BLE001 -- RPC errors must be returned to the owner.
                import traceback

                traceback.print_exc(file=sys.stderr)
                result = {"error": repr(error)}
            print(json.dumps(result), flush=True)
    finally:
        agent.close()


def main():
    parser = argparse.ArgumentParser(__doc__)
    parser.add_argument("--build-info", required=True)
    parser.add_argument("--agent", action="store_true")
    parser.add_argument("--config")
    parser.add_argument("--output")
    parser.add_argument("--cases", help="Comma-separated override of config.cases")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    if args.agent:
        serve(json.loads(Path(args.build_info).read_text()))
    else:
        if not args.config or not args.output:
            parser.error("--config and --output required")
        output = Path(args.output).absolute()
        output.mkdir(parents=True, exist_ok=False)
        config = json.loads(Path(args.config).read_text())
        if args.cases:
            config["cases"] = args.cases.split(",")
        write_json(output / "config.json", config)
        Experiment(config, args.build_info, output).run()


if __name__ == "__main__":
    main()
