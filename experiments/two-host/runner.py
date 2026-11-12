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

from agent import (
    CLONE,
    IMAGE,
    Agent,
    image_id,
    parse_block_stat,
    stop,
    wait_for,
    write_json,
)

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
    # Concurrent workload guests per member for the reads cases; one arm per entry.
    "guests_per_host": [1],
    # Workload guest sizing; the pinned build defaults to two vCPUs and 1 GiB.
    "guest": {"cores": 2, "memory_mib": 1024},
    "burst": {},
    "resume": {},
}
BURST_DEFAULTS = {
    "images": 4,
    "base_bytes": 64 * 1024**2,
    "unique_bytes": 8 * 1024**2,
    "k": 1,
    "force_remote": False,
    "owner_cache_bytes": 0,
    "drop_page_cache": False,
    "guest": None,
}
RESUME_DEFAULTS = {
    "setup_bytes": None,
    "setup_script": "",
    "k": 1,
    "settle": True,
    "drop_page_cache": False,
    "guest": None,
}
DEFAULT_CASES = [
    "flows-k1",
    "flows-k2",
    "reads-memory",
    "reads-storage",
    "raw-rsync",
    "zfs-incremental",
    "independent-stores",
]
MAX_GUESTS = 16
MAX_BURST_IMAGES = 32
BASE_SEED = 1000
UNIQUE_SEED = 2000
RESUME_SEED = 4242
PEER_COUNTERS = (
    "gets",
    "verified",
    "errors",
    "connections",
    "server_memory",
    "server_store",
    "server_missing",
    "server_errors",
    "has",
    "completed_reads",
    "failed_reads",
    "inline_attempts",
    "inline_completed",
)


def sizing(value):
    if not isinstance(value, dict) or set(value) != {"cores", "memory_mib"}:
        raise ValueError("guest sizing needs exactly cores and memory_mib")
    cores, memory = int(value["cores"]), int(value["memory_mib"])
    if cores < 1 or memory < 128:
        raise ValueError("guest sizing needs at least one core and 128 MiB")
    return {"cores": cores, "memory_mib": memory}


def normalize_settings(config):
    """Merge defaults and fail on inconsistent or mislabelable settings."""
    settings = {**DEFAULTS, **config.get("settings", {})}
    members = len(config["nodes"])
    if not (0 < settings["data_bytes"] < settings["image_bytes"] // 2):
        raise ValueError(
            "data must fit the guest image with filesystem and drift headroom"
        )
    if not 1 <= settings["seconds"] <= 300 or not 1 <= settings["repeats"] <= 20:
        raise ValueError("invalid run duration/repeat count")
    guests = settings["guests_per_host"]
    guests = [guests] if isinstance(guests, int) else list(guests)
    if (
        not guests
        or any(not isinstance(n, int) or not 1 <= n <= MAX_GUESTS for n in guests)
        or len(set(guests)) != len(guests)
    ):
        raise ValueError(
            f"guests_per_host needs distinct integers from 1 to {MAX_GUESTS}"
        )
    settings["guests_per_host"] = guests
    if settings["cache_bytes"] < settings["data_bytes"] * 2 * max(guests):
        raise ValueError(
            "memory cohort needs cache headroom for every guest's data and metadata"
        )
    settings["guest"] = sizing(settings["guest"])
    burst = {**BURST_DEFAULTS, **settings["burst"]}
    if set(burst) != set(BURST_DEFAULTS):
        raise ValueError(
            f"unknown burst settings: {sorted(set(burst) - set(BURST_DEFAULTS))}"
        )
    if not 1 <= burst["images"] <= MAX_BURST_IMAGES:
        raise ValueError(f"burst images must be 1 to {MAX_BURST_IMAGES}")
    for key in ("base_bytes", "unique_bytes"):
        if burst[key] <= 0 or burst[key] % 4096:
            raise ValueError(f"burst {key} must be a positive 4 KiB multiple")
    if burst["base_bytes"] + burst["unique_bytes"] >= settings["image_bytes"] // 2:
        raise ValueError("burst regions must fit the image with filesystem headroom")
    if not 1 <= burst["k"] <= members:
        raise ValueError("burst k must be between 1 and the member count")
    if burst["force_remote"] and burst["k"] < 2:
        raise ValueError(
            "force_remote needs a nonlocal owner for every chunk: burst k must be at least 2"
        )
    burst["guest"] = sizing(burst["guest"] or settings["guest"])
    settings["burst"] = burst
    resume = {**RESUME_DEFAULTS, **settings["resume"]}
    if set(resume) != set(RESUME_DEFAULTS):
        raise ValueError(
            f"unknown resume settings: {sorted(set(resume) - set(RESUME_DEFAULTS))}"
        )
    if resume["setup_bytes"] is None:
        resume["setup_bytes"] = settings["data_bytes"]
    if (
        resume["setup_bytes"] <= 0
        or resume["setup_bytes"] % 4096
        or resume["setup_bytes"] >= settings["image_bytes"] // 2
    ):
        raise ValueError(
            "resume setup_bytes must be a 4 KiB multiple that fits the image"
        )
    if not 1 <= resume["k"] <= members:
        raise ValueError("resume k must be between 1 and the member count")
    if not isinstance(resume["setup_script"], str):
        raise TypeError("resume setup_script must be a shell string")
    resume["guest"] = sizing(resume["guest"] or settings["guest"])
    settings["resume"] = resume
    return settings


def plan_cases(config, settings):
    """Expand the case list into (case name, handler, argument) rows.

    `reads-memory` runs one arm per `guests_per_host` entry; the one-guest arm
    keeps its historical name and schema, larger arms are suffixed `-gN`.
    """
    plan = []
    for case in config.get("cases", DEFAULT_CASES):
        match = re.fullmatch(r"reads-(memory|storage)(?:-g(\d+))?", case)
        if match:
            target, explicit = match[1], match[2]
            arms = [int(explicit)] if explicit else settings["guests_per_host"]
            for guests in arms:
                if not 1 <= guests <= MAX_GUESTS:
                    raise ValueError(f"unsupported guest count in {case}")
                plan.append((arm_name(target, guests), "reads", (target, guests)))
        elif case in ("flows-k1", "flows-k2"):
            plan.append((case, "flows", int(case[-1])))
        elif case in ("raw-rsync", "zfs-incremental", "independent-stores"):
            plan.append((case, case.replace("-", "_"), None))
        elif case == "burst-images":
            plan.append((case, "burst", None))
        elif case == "resume-elsewhere":
            plan.append((case, "resume", None))
        else:
            raise ValueError(f"unknown case {case}")
    if len({name for name, _, _ in plan}) != len(plan):
        raise ValueError("duplicate case arms in plan")
    return plan


def arm_name(target, guests):
    return f"reads-{target}" if guests == 1 else f"reads-{target}-g{guests}"


def guest_names(prefix, count):
    return [prefix] if count == 1 else [f"{prefix}-{i}" for i in range(count)]


def merge_bins(bins):
    merged = {}
    for table in bins:
        for key, count in table.items():
            merged[key] = merged.get(key, 0) + count
    return dict(sorted(merged.items(), key=lambda item: int(item[0])))


def bin_percentiles(bins, percentiles=(50.0, 95.0, 99.0, 99.9)):
    total = sum(bins.values())
    if not total:
        raise ValueError("no samples in merged latency bins")
    result = {}
    cumulative = 0
    items = sorted(((int(k), v) for k, v in bins.items()), key=lambda item: item[0])
    for percentile in percentiles:
        target = percentile / 100 * total
        cumulative = 0
        for value, count in items:
            cumulative += count
            if cumulative >= target:
                result[f"{percentile:.6f}"] = value
                break
    return result


def aggregate_fio(results):
    """Combine per-guest fio json+ jobs: summed throughput, merged histograms.

    Percentiles come from the merged json+ bins, so they are bucket lower
    bounds rather than fio's exact per-job percentile values.
    """
    jobs = [job for result in results for job in result["jobs"]]
    if not jobs:
        raise ValueError("no fio jobs to aggregate")
    aggregate = {"guests": len(results), "jobs": len(jobs), "errors": 0}
    for direction in ("read", "write"):
        rows = [job[direction] for job in jobs]
        summary = {
            "io_bytes": sum(r["io_bytes"] for r in rows),
            "total_ios": sum(r["total_ios"] for r in rows),
            "iops": sum(r["iops"] for r in rows),
            "bw_bytes": sum(r["bw_bytes"] for r in rows),
            "runtime_ms_max": max(r["runtime"] for r in rows),
        }
        for key in ("lat_ns", "clat_ns"):
            tables = [r[key]["bins"] for r in rows if "bins" in r.get(key, {})]
            if tables and summary["total_ios"]:
                bins = merge_bins(tables)
                summary[key] = {
                    "bins": bins,
                    "N": sum(bins.values()),
                    "min": min(r[key]["min"] for r in rows),
                    "max": max(r[key]["max"] for r in rows),
                    "percentile": bin_percentiles(bins),
                }
        aggregate[direction] = summary
        aggregate["errors"] += sum(1 for job in jobs if job["error"])
    if aggregate["errors"]:
        raise ValueError("aggregated fio jobs include errors")
    return aggregate


def peer_counters(before, after):
    result = {}
    for key in PEER_COUNTERS:
        delta = after["peer"]["counters"][key] - before["peer"]["counters"][key]
        if delta < 0:
            raise ValueError(f"peer counter {key} reset inside a measurement")
        result[key] = delta
    result["get_wire"] = {
        k: after["peer"]["get_wire"][k] - before["peer"]["get_wire"][k]
        for k in ("sent", "received")
    }
    result["max_inflight"] = {
        "before": before["peer"]["counters"]["max_inflight"],
        "after": after["peer"]["counters"]["max_inflight"],
    }
    return result


def ring_since(report):
    return max((r["completed_ns"] for r in report["peer"]["recent"]), default=0)


def burst_guest_summary(guest, marker, chunk_bytes=4096):
    """Per-guest boot milestones and guest-observed read fraction.

    The daemon's GET counters are host wide, so a per-image fetched fraction
    is guest-observed device reads over the image's used ext4 blocks. Under
    force-remote every settled read is a GET; otherwise locally owned chunks
    are read locally and the host-level GET delta bounds remote fetches.
    """
    lines = guest["probe"].splitlines()
    if len(lines) < 3 or lines[0] != marker:
        raise ValueError(f"guest {guest['name']} marker probe returned {lines[:1]}")
    device = parse_block_stat(lines[1])
    blocks, free, fragment = (int(v) for v in lines[2].split())
    used_chunks = (blocks - free) * fragment // chunk_bytes
    read_chunks = device["read_sectors"] * 512 // chunk_bytes
    origin = guest["launched_ns"]
    return {
        "name": guest["name"],
        "image": guest["image"],
        "ready_ns": guest["ready_ns"] - origin,
        "ssh_banner_ns": guest["ssh_banner_ns"] - origin,
        "first_command_ns": guest["first_command_ns"] - origin,
        "marker_ns": guest["probe_ns"] - origin,
        "device_at_marker": device,
        "used_chunks": used_chunks,
        "read_chunks_at_marker": read_chunks,
        "read_fraction_at_marker": read_chunks / used_chunks if used_chunks else None,
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
        timeout = 900
        if method == "boot":
            timeout = 2400
        elif method == "boot_burst":
            timeout = 2400 + 600 * len(args["guests"])
        value = self.replies.get(timeout=timeout)
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
        if not 2 <= len(config["nodes"]) <= 16:
            raise ValueError("runner supports 2 through 16 nodes")
        self.settings = normalize_settings(config)
        self.plan = plan_cases(config, self.settings)
        self.output = output
        self.agents = []
        for index, node in enumerate(config["nodes"]):
            directory = output / str(index)
            directory.mkdir()
            self.agents.append(Remote(node, build_info, directory))
        self.results = {
            "schema_version": 2,
            "config": config,
            "settings": self.settings,
            "planned_cases": [name for name, _, _ in self.plan],
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

    def start(self, name, k=None, cache=0, force=False, guests=1):
        works = parallel(self.agents, lambda agent: agent.call("start_case", name=name))
        self.sources = [image_id(IMAGE, i) for i in range(guests)]
        self.clones = [image_id(CLONE, i) for i in range(guests)]
        if k:
            parallel(
                range(len(self.agents)),
                lambda n: self.agents[n].call(
                    "init", node=n, images=self.sources if n == 0 else None
                ),
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
                    images=self.sources + self.clones if n < 2 else [],
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

    def boot(self, node, name, **kwargs):
        kwargs.setdefault("sizing", self.settings["guest"])
        return self.agents[node].call("boot", name=name, **kwargs)

    def seed(self, backend="cas"):
        self.boot(0, "source", backend=backend, format=True)
        self.fio(0, "source", "seed", "seed")
        return self.epoch_digest(0, "source")

    def epoch_digest(self, node, guest):
        return (
            self.agents[node]
            .call(
                "shell",
                guest=guest,
                script="cd /mnt/cas; sha256sum data; echo 0 > epoch; sync -f .",
            )
            .split()[0]
        )

    def boot_burst(self, node, names, images, sizing, probe=None, format=False):
        return self.agents[node].call(
            "boot_burst",
            guests=[
                {"name": name, "image": image, "sizing": sizing, "format": format}
                for name, image in zip(names, images, strict=True)
            ],
            probe=probe,
        )

    def fio_many(self, node, guests, kind, tag, **kwargs):
        agent = self.agents[node]
        names = [tag if len(guests) == 1 else f"{tag}-{guest}" for guest in guests]
        for guest, name in zip(guests, names, strict=True):
            agent.call("fio", guest=guest, kind=kind, name=name, **kwargs)
        return [agent.call("finish_fio", name=name) for name in names]

    def seed_many(self, guests):
        """Boot `guests` sources on A concurrently and seed each one."""
        names = guest_names("source", guests)
        if guests == 1:
            return names, [self.seed()]
        boots = self.boot_burst(
            0, names, self.sources, self.settings["guest"], format=True
        )
        self.fio_many(0, names, "seed", "seed")
        self.results["cases"][self.case]["source_boot"] = boots
        return names, [self.epoch_digest(0, name) for name in names]

    def verify(self, node, guest, digest, continue_work=False):
        script = f'cd /mnt/cas; echo {shlex.quote(digest + "  data")} | sha256sum -c -; test "$(cat epoch)" = 0'
        if continue_work:
            script += '; echo 1 > epoch; sync -f .; test "$(cat epoch)" = 1'
        return self.agents[node].call(
            "shell", guest=guest, script=script + "; sync -f ."
        )

    def provision(self, sources=("source",)):
        """Freeze every source, copy each settled manifest to B, then thaw."""
        a = self.agents[0]
        a.call("settle")
        self.sync()
        frozen = []
        images = []
        before = self.report()
        start = time.monotonic_ns()
        try:
            for guest in sources:
                a.call("shell", guest=guest, script="fsfreeze -f /mnt/cas")
                frozen.append(guest)
            for index, _ in enumerate(sources):
                began = time.monotonic_ns()
                result = self.fleet(
                    "provision",
                    source=self.config["nodes"][0]["bulk"],
                    destination=self.config["nodes"][1]["bulk"],
                    image=self.sources[index],
                    target=self.clones[index],
                )
                images.append(
                    {"daemon": result, "elapsed_ns": time.monotonic_ns() - began}
                )
        finally:
            for guest in frozen:
                a.call("shell", guest=guest, script="fsfreeze -u /mnt/cas")
        result = {
            "elapsed_ns": time.monotonic_ns() - start,
            "wire": deltas(before, self.report()),
        }
        if len(sources) == 1:
            return {"daemon": images[0]["daemon"], **result}
        return {"images": images, **result}

    def flows(self, k):
        name = f"flows-k{k}"
        self.start(name, k=k)
        result = self.results["cases"][name] = {}
        a, b = self.agents[:2]
        digest = self.seed()
        result["provision"] = self.provision()
        start = time.monotonic_ns()
        self.boot(1, "clone", image=CLONE)
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
        self.boot(1, "resumed", image=IMAGE)
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

    def observed_read(self, kind, name, depth=1, readers=("reader",)):
        before = self.report()
        last = before
        samples = []
        names = [name if len(readers) == 1 else f"{name}-{guest}" for guest in readers]
        for guest, job in zip(readers, names, strict=True):
            self.agents[1].call("fio", guest=guest, kind=kind, name=job, depth=depth)
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
            if not any(self.agents[1].call("fio_running", name=job) for job in names):
                break
        jobs = [self.agents[1].call("finish_fio", name=job) for job in names]
        if len(readers) == 1:
            return jobs[0], before, self.report(), samples
        fio = aggregate_fio(jobs)
        fio["guests"] = [
            {"name": guest, "fio": job}
            for guest, job in zip(readers, jobs, strict=True)
        ]
        return fio, before, self.report(), samples

    def reads(self, arm):
        target, guests = arm
        name = arm_name(target, guests)
        self.case = name
        self.start(
            name,
            k=len(self.agents),
            cache=self.settings["cache_bytes"] if target == "memory" else 0,
            force=True,
            guests=guests,
        )
        result = self.results["cases"][name] = {"cohorts": []}
        if guests > 1:
            result["guests_per_host"] = guests
            result["guest_sizing"] = self.settings["guest"]
        a, b = self.agents[:2]
        sources, digests = self.seed_many(guests)
        result["provision"] = self.provision(sources)
        readers = guest_names("reader", guests)
        if guests == 1:
            self.boot(1, "reader", image=CLONE)
        else:
            result["reader_boot"] = self.boot_burst(
                1, readers, self.clones, self.settings["guest"]
            )
        for reader, digest in zip(readers, digests, strict=True):
            self.verify(1, reader, digest)
        # Read every block to warm the owner; B never fills its cache.
        self.fio_many(1, readers, "verify", "warm")
        for repeat in range(self.settings["repeats"]):
            for depth in self.settings["depths"]:
                for kind in ["random", "sequential"] if depth == 1 else ["sequential"]:
                    tag = f"r{repeat}-{kind}-qd{depth}"
                    fio, before, after, samples = self.observed_read(
                        kind, tag, depth, readers
                    )
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
            # Every source guest ships; the aggregate requested rate stays fixed.
            tag = f"r{repeat}-contention"
            writers = [
                tag + "-writer" if guests == 1 else f"{tag}-writer-{source}"
                for source in sources
            ]
            for source, writer in zip(sources, writers, strict=True):
                a.call(
                    "fio",
                    guest=source,
                    kind="ship",
                    name=writer,
                    seconds=self.settings["seconds"] + 10,
                    depth=4,
                    rate=self.settings["ship_rate_bytes"] // guests,
                )
            fio, before, after, samples = self.observed_read("random", tag, 1, readers)
            if not has_overlap(samples):
                raise ValueError(
                    "contention run has no observed overlap of acknowledged PUT chunks and verified GETs"
                )
            finished = [a.call("finish_fio", name=writer) for writer in writers]
            writer = finished[0] if guests == 1 else finished
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
            self.fio_many(1, readers, "verify", tag + "-rewarm")
        b.call("stop_guests", names=readers)
        a.call("stop_guests", names=sources)
        self.end()

    def cold_member(self, node, images, drop_page_cache):
        """Restart one member's daemon cold with an empty clean cache."""
        agent = self.agents[node]
        cut = time.monotonic_ns()
        restart = agent.call("restart_host", images=images)

        def recovered():
            try:
                return agent.call("report")
            except RuntimeError as error:
                if "cas-host exited" in str(error):
                    raise
                return False

        wait_for(recovered, 120)
        result = {
            "restart": restart,
            "cold_recovery_ns": time.monotonic_ns() - cut,
            "page_cache": None,
        }
        if drop_page_cache:
            # Fails loudly without the privilege instead of mislabelling the run.
            result["page_cache"] = agent.call("drop_page_cache")
        return result

    def burst(self):
        name = "burst-images"
        burst = self.settings["burst"]
        n = burst["images"]
        self.start(
            name,
            k=burst["k"],
            cache=burst["owner_cache_bytes"],
            force=burst["force_remote"],
            guests=n,
        )
        result = self.results["cases"][name] = {
            "images": n,
            "k": burst["k"],
            "force_remote": burst["force_remote"],
            "owner_cache_bytes": burst["owner_cache_bytes"],
            "guest_sizing": burst["guest"],
            "base_bytes": burst["base_bytes"],
            "unique_bytes": burst["unique_bytes"],
            "attribution": (
                "GET/HAS counters are per member; per-image read fractions are "
                "guest-observed device reads over used ext4 blocks"
            ),
        }
        a, b = self.agents[:2]
        sources = guest_names("source", n)
        result["source_boot"] = self.boot_burst(
            0, sources, self.sources, burst["guest"], format=True
        )
        # Every image shares the seeded base region and differs in its unique
        # region, whose first block carries the marker read at boot on B.
        contents = []
        chunks_before = a.call("report")["store"]["chunks"]
        for index, source in enumerate(sources):
            marker = f"cas burst image {index} marker"
            base = a.call(
                "write_payload",
                guest=source,
                name="base",
                seed=BASE_SEED,
                size=burst["base_bytes"],
            )
            unique = a.call(
                "write_payload",
                guest=source,
                name="unique",
                seed=UNIQUE_SEED + index,
                size=burst["unique_bytes"],
                marker=marker,
            )
            a.call("settle")
            chunks = a.call("report")["store"]["chunks"]
            contents.append(
                {
                    "image": self.sources[index],
                    "base": base,
                    "unique": unique,
                    "marker": marker,
                    "store_chunk_delta": chunks - chunks_before,
                }
            )
            chunks_before = chunks
        result["contents"] = contents
        self.save()
        result["provision"] = self.provision(sources)
        result["source_shutdown"] = a.call("stop_guests", names=sources)
        result["destination_cold"] = self.cold_member(
            1, self.clones, burst["drop_page_cache"]
        )
        before = self.report()
        since = ring_since(before[1])
        names = guest_names("burst", n)
        probe = (
            "dd if=/mnt/cas/unique bs=4096 count=1 iflag=direct status=none | tr -d '\\000'; echo;"
            " cat /sys/block/vda/stat; stat -f -c '%b %f %S' /mnt/cas"
        )
        guests = self.boot_burst(1, names, self.clones, burst["guest"], probe)
        at_marker = self.report()
        result["guests"] = [
            burst_guest_summary(guest, content["marker"])
            for guest, content in zip(guests, contents, strict=True)
        ]
        result["guest_boots"] = guests
        launches = [guest["launched_ns"] for guest in guests]
        result["launch_spread_ns"] = max(launches) - min(launches)
        # Full verification of both regions after the boot window.
        verified = []
        for guest, content in zip(names, contents, strict=True):
            began = time.monotonic_ns()
            output = b.call(
                "shell", guest=guest, script="cd /mnt/cas; sha256sum base unique"
            ).split()
            if output[0::2] != [content["base"]["sha256"], content["unique"]["sha256"]]:
                raise ValueError(f"burst guest {guest} content differs from its source")
            verified.append(
                {
                    "name": guest,
                    "verify_ns": time.monotonic_ns() - began,
                    "stat": b.call("guest_stat", guest=guest),
                }
            )
        result["verified"] = verified
        after = self.report()
        used = sum(guest["used_chunks"] for guest in result["guests"])
        windows = {}
        for label, end in (("to_marker", at_marker), ("to_verified", after)):
            peers = [peer_counters(x, y) for x, y in zip(before, end, strict=True)]
            windows[label] = {
                "wire": deltas(before, end),
                "peer": peers,
                "destination_fetched_chunks": peers[1]["gets"],
                "fetched_fraction_of_used": peers[1]["gets"] / used if used else None,
            }
            if any(p["errors"] or p["failed_reads"] for p in peers):
                raise ValueError("remote read errors during the burst window")
        if windows["to_marker"]["destination_fetched_chunks"] == 0:
            raise ValueError("burst window observed no GETs on the destination")
        result["windows"] = windows
        result["recent_phases"] = phases(after[1], since)
        result["before"] = before
        result["after"] = after
        result["loadavg"] = os.getloadavg()
        b.call("stop_guests", names=names)
        self.end()

    def resume(self):
        name = "resume-elsewhere"
        resume = self.settings["resume"]
        self.start(name, k=resume["k"])
        result = self.results["cases"][name] = {
            "k": resume["k"],
            "settled_before_shutdown": resume["settle"],
            "guest_sizing": resume["guest"],
            "setup_bytes": resume["setup_bytes"],
            "setup_script": resume["setup_script"],
        }
        a, b = self.agents[:2]
        self.boot(0, "source", format=True, sizing=resume["guest"])
        marker = "cas resume marker"
        result["setup"] = a.call(
            "write_payload",
            guest="source",
            name="data",
            seed=RESUME_SEED,
            size=resume["setup_bytes"],
            marker=marker,
        )
        if resume["setup_script"]:
            began = time.monotonic_ns()
            result["setup_script_output"] = a.call(
                "shell", guest="source", script=resume["setup_script"]
            )
            result["setup_script_ns"] = time.monotonic_ns() - began
        digest = self.epoch_digest(0, "source")
        result["verified_sha256"] = digest
        if resume["settle"]:
            a.call("settle")
            result["placement_sync"] = self.sync()
        result["before_shutdown"] = self.report()
        start = time.monotonic_ns()
        shutdown = a.call("stop_guest", name="source")
        cut = time.monotonic_ns()
        cold = parallel(
            range(2),
            lambda n: self.cold_member(
                n, [IMAGE] if n == 1 else [], resume["drop_page_cache"] and n == 1
            ),
        )
        recovered_at = time.monotonic_ns()
        before = self.report()
        since = ring_since(before[1])
        transfer = self.fleet(
            "migrate",
            source=self.config["nodes"][0]["bulk"],
            destination=self.config["nodes"][1]["bulk"],
            image=IMAGE,
        )
        migrated = self.report()
        boot = time.monotonic_ns()
        probe = (
            "dd if=/mnt/cas/data bs=4096 count=1 iflag=direct status=none | tr -d '\\000'; echo;"
            " cat /sys/block/vda/stat; stat -f -c '%b %f %S' /mnt/cas"
        )
        guest = self.boot_burst(1, ["resumed"], [IMAGE], resume["guest"], probe)[0]
        summary = burst_guest_summary(guest, marker)
        self.verify(1, "resumed", digest, continue_work=True)
        end = time.monotonic_ns()
        verified = self.report()
        post_sync = self.fleet("sync", address=self.config["nodes"][1]["bulk"])
        after = self.report()
        result["migration"] = {
            "downtime_ns": end - start,
            "stop_ns": cut - start,
            "handoff_ns": boot - cut,
            "cold_recovery_ns": recovered_at - cut,
            "boot_verify_continue_ns": end - boot,
            "first_command_ns": guest["first_command_ns"] - boot,
            "marker_ns": guest["probe_ns"] - boot,
            "guest": summary,
            "shutdown": shutdown,
            "cold": cold,
            "daemon": transfer,
            "verified_sha256": digest,
            "continued_epoch": 1,
        }
        result["phases"] = {
            "manifest_bytes": transfer["result"]["manifest_bytes"],
            "staging_tail_bytes": transfer["result"]["tail_bytes"],
            "migration_wire": deltas(before, migrated),
            "missing_chunks": {
                "boot_verify_wire": deltas(migrated, verified),
                "peer": [
                    peer_counters(x, y) for x, y in zip(migrated, verified, strict=True)
                ],
                "explicit_sync": post_sync,
                "explicit_sync_wire": deltas(verified, after),
            },
        }
        if any(p["errors"] for p in result["phases"]["missing_chunks"]["peer"]):
            raise ValueError("remote read errors while resuming on B")
        result["recent_phases"] = phases(after[1], since)
        result["after"] = after
        b.call("stop_guest", name="resumed")
        self.end()

    def independent_stores(self):
        self.start("independent-stores")
        parallel(self.agents, lambda agent: agent.call("init", node=0))
        parallel(
            range(len(self.agents)),
            lambda n: self.agents[n].call(
                "host", node=n, k=0, members=self.config["nodes"], cache=0
            ),
        )
        parallel(range(len(self.agents)), lambda n: self.boot(n, "guest", format=True))
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

    def raw_rsync(self):
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
        self.boot(1, "clone", image=CLONE, backend="raw")
        self.verify(1, "clone", digest)
        result["provision"]["shutdown_copy_boot_verify_ns"] = (
            time.monotonic_ns() - provision_start
        )
        # Restart A to give the raw arm the same running-guest starting condition.
        self.boot(0, "drift-source", backend="raw")
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
        self.boot(1, "resumed", backend="raw")
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
        self.boot(1, "resynced", image=CLONE, backend="raw")
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

    def zfs_incremental(self):
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
        self.boot(0, "updated-source", backend="raw")
        a.call("shell", guest="updated-source", script="fsfreeze -f /mnt/cas")
        try:
            provision = a.call("zfs_snapshot", name="provision")
        finally:
            a.call("shell", guest="updated-source", script="fsfreeze -u /mnt/cas")
        result["provision"] = self.zfs_transfer(provision, base)
        start = time.monotonic_ns()
        self.boot(1, "clone", image=IMAGE, backend="raw")
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
        self.boot(1, "synced", image=IMAGE, backend="raw")
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
        self.boot(1, "resumed", image=IMAGE, backend="raw")
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
            for case, handler, argument in self.plan:
                print(f"Running {case}", file=sys.stderr, flush=True)
                self.case = case
                method = getattr(self, handler)
                if argument is None:
                    method()
                else:
                    method(argument)
            self.results["success"] = True
        except BaseException as error:
            self.results.update(success=False, error=repr(error))
            raise
        finally:
            self.results["finished_unix_ns"] = time.time_ns()
            self.save()
            for agent in self.agents:
                agent.close()


def apply_overrides(config, overrides):
    """`KEY=JSON` settings overrides; the effective config is retained."""
    settings = dict(config.get("settings", {}))
    for item in overrides:
        key, separator, value = item.partition("=")
        if not separator or not key or key not in DEFAULTS:
            raise ValueError(f"override must name a known setting: {item}")
        settings[key] = json.loads(value)
    return {**config, "settings": settings}


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
    parser.add_argument(
        "--set",
        action="append",
        default=[],
        metavar="KEY=JSON",
        help="Override one settings entry, e.g. guests_per_host=[4]",
    )
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
        config = apply_overrides(config, args.set)
        write_json(output / "config.json", config)
        Experiment(config, args.build_info, output).run()


if __name__ == "__main__":
    main()
