"""Owned host-side operations for the two-host experiment (stdlib only)."""

import hashlib
import json
import os
import random
import resource
import shlex
import shutil
import signal
import socket
import subprocess
import tempfile
import time
from pathlib import Path

STORE = "01" * 16
IMAGE = "02" * 16
EMPTY = "03" * 16
CLONE = "04" * 16


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + "\n")


def checked(argv, **kwargs):
    result = subprocess.run(
        argv, check=False, text=True, capture_output=True, timeout=600, **kwargs
    )
    if result.returncode:
        raise RuntimeError(
            f"{argv}: exit {result.returncode}\n{result.stdout}\n{result.stderr}"
        )
    return result.stdout


def stop(process):
    # A launcher may have exited while children remain in its owned group.
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=10)


def wait_for(predicate, seconds=180):
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        value = predicate()
        if value:
            return value
        time.sleep(0.05)
    raise TimeoutError("experiment readiness deadline")


def fio_options(kind, size, seconds, depth=1, seed=42):
    options = [
        "fio",
        "--name=data",
        "--filename=/mnt/cas/data",
        f"--size={size}",
        "--ioengine=libaio",
        "--direct=1",
        "--bs=4096",
        f"--iodepth={depth}",
        "--verify=crc32c",
        "--verify_fatal=1",
        "--verify_dump=1",
        "--group_reporting=1",
        "--output-format=json+",
        "--lat_percentiles=1",
        "--percentile_list=50:95:99:99.9",
        f"--randseed={seed}",
    ]
    if kind in ("seed", "drift"):
        options += ["--rw=write", "--do_verify=1", "--end_fsync=1"]
        if kind == "drift":
            options += ["--filename=/mnt/cas/drift"]
    elif kind == "ship":
        options += [
            "--filename=/mnt/cas/ship",
            "--rw=write",
            "--refill_buffers=1",
            "--verify=0",
            "--end_fsync=1",
            "--fsync=16",
            "--time_based=1",
            f"--runtime={seconds}",
        ]
    else:
        options += ["--rw=" + ("randread" if kind == "random" else "read")]
        if kind != "verify":
            options += ["--time_based=1", f"--runtime={seconds}"]
    return options


def qemu_record(pid, acceleration):
    pending = [pid]
    for current in pending:
        if len(pending) > 64:
            break
        root = Path(f"/proc/{current}")
        try:
            executable = os.readlink(root / "exe")
            if "qemu-system-" in Path(executable).name:
                argv = (root / "cmdline").read_bytes().decode().strip("\0").split("\0")
                descriptors = []
                for fd in (root / "fd").iterdir():
                    try:
                        descriptors.append(os.readlink(fd))
                    except FileNotFoundError:
                        pass
                kvm = any("kvm-vm" in fd for fd in descriptors)
                if kvm != (acceleration == "kvm"):
                    raise ValueError(
                        "QEMU's live KVM descriptors disagree with the selected build"
                    )
                return {
                    "pid": current,
                    "executable": executable,
                    "argv": argv,
                    "kvm_vm_fd": kvm,
                    "process_status": (root / "status").read_text(),
                }
            pending.extend(
                int(child)
                for child in (root / f"task/{current}/children").read_text().split()
            )
        except FileNotFoundError:
            continue
    raise RuntimeError("could not identify the running QEMU process")


class Agent:
    def __init__(self, build):
        self.build = build
        self.children = {}
        self.guests = {}
        self.work = None
        self.sockets = tempfile.TemporaryDirectory(prefix="cas-fleet-")
        self.serial = 0
        self.case = None

    def spawn(self, name, argv, env=None, pin=None):
        if name in self.children:
            raise ValueError(f"child already exists: {name}")
        if pin:
            argv = ["taskset", "-c", pin, *argv]
        write_json(self.case / f"{name}-command.json", {"argv": argv, "env": env})
        with (self.case / f"{name}.log").open("x") as log:
            process = subprocess.Popen(
                argv,
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=log,
                env={**os.environ, **(env or {})},
                start_new_session=True,
            )
        self.children[name] = process
        return process

    def healthy(self):
        host = self.children.get("host")
        if host and host.poll() is not None:
            raise RuntimeError(
                f"cas-host exited {host.returncode}; see {self.case}/host.log"
            )
        return True

    def configure(self, node, output, settings):
        soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
        if soft != resource.RLIM_INFINITY and soft < 8192:
            resource.setrlimit(resource.RLIMIT_NOFILE, (8192, hard))
        self.node, self.settings = node, settings
        self.output = Path(output)
        self.output.mkdir(parents=True, exist_ok=False)
        self.storage = Path(node["storage"])
        if not self.storage.is_absolute() or self.storage.exists():
            raise ValueError("storage must be a fresh absolute directory")
        parent = self.storage.parent
        mount = json.loads(
            checked(
                ["findmnt", "--json", "-T", str(parent), "-o", "FSTYPE,SOURCE,TARGET"]
            )
        )["filesystems"][0]
        if mount["fstype"] != "xfs":
            raise ValueError(
                "each host requires a dedicated, already mounted XFS filesystem"
            )
        if parent.stat().st_dev == self.output.stat().st_dev:
            raise ValueError("evidence must be outside the storage filesystem")
        self.storage.mkdir()
        self.key = Path(self.sockets.name) / "id"
        checked(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(self.key)])
        identity = {
            "build": self.build,
            "uname": os.uname(),
            "mount": mount,
            "affinity": sorted(os.sched_getaffinity(0)),
            "loadavg": os.getloadavg(),
            "nofile": resource.getrlimit(resource.RLIMIT_NOFILE),
            "tools": {},
            "hardware": {},
        }
        for tool in ("host", "fleet"):
            identity[tool + "_sha256"] = hashlib.sha256(
                Path(self.build[tool]).read_bytes()
            ).hexdigest()
        for name, argv in {
            "lsblk": ["lsblk", "-J", "-b", "-O"],
            "network": ["ip", "-j", "address"],
            "links": ["ip", "-j", "-s", "link"],
            "cpu": ["lscpu", "-J"],
            "qemu": [self.build["qemu"], "--version"],
            "interrupts": ["cat", "/proc/interrupts"],
        }.items():
            try:
                identity["hardware"][name] = checked(argv)
            except (RuntimeError, FileNotFoundError) as error:
                identity["hardware"][name] = {"unavailable": str(error)}
        write_json(self.output / "identity.json", identity)
        return identity

    def start_case(self, name):
        if self.work is not None:
            raise ValueError("previous case still owned")
        if not name.replace("-", "").isalnum():
            raise ValueError("invalid case name")
        self.case = self.output / name
        self.case.mkdir()
        self.work = self.storage / name
        self.work.mkdir()
        return str(self.work)

    def storage_args(self):
        return [
            "--root",
            str(self.work / "store"),
            "--store",
            STORE,
            "--segment-bytes",
            str(self.settings["segment_bytes"]),
            "--staging-bytes",
            str(self.settings["staging_bytes"]),
        ]

    def init(self, node):
        root = self.work / "store"
        root.mkdir()
        return json.loads(
            checked(
                [
                    self.build["host"],
                    "init",
                    *self.storage_args(),
                    "--image",
                    f"{IMAGE if node == 0 else EMPTY}={self.settings['image_bytes']}",
                ]
            )
        )

    def host(self, node, k, peer, cache, force=False):
        reports = self.case / "daemon"
        reports.mkdir()
        argv = [
            self.build["host"],
            *self.storage_args(),
            "--mode",
            "cold",
            "--reports",
            str(reports),
            "--telemetry",
            "--telemetry-rotate",
            "--cache-bytes",
            str(cache),
        ]
        if k:
            argv += [
                "--peer-node",
                str(node),
                "--peer-k",
                str(k),
                "--peer-listen",
                self.node["get"],
                "--peer-address",
                peer["get"],
                "--peer-bulk-listen",
                self.node["bulk"],
                "--peer-bulk-address",
                peer["bulk"],
                "--peer-inline-us",
                str(self.settings["inline_us"]),
            ]
        if force:
            argv.append("--peer-force-remote")
        for image in (IMAGE, CLONE) if k else (IMAGE,):
            argv += ["--image", f"{image}={self.socket(image)}"]
        self.spawn("host", argv, pin=self.node.get("cpus"))
        return {"pid": self.children["host"].pid}

    def socket(self, image):
        return str(Path(self.sockets.name) / f"{self.case.name}-{image}.sock")

    def report(self):
        self.healthy()
        return self.fleet(["report", "--address", self.node["bulk"]])["result"]

    def fleet(self, argv):
        return json.loads(checked([self.build["fleet"], *argv]))

    def boot(self, name, image=IMAGE, backend="cas", format=False):
        self.healthy()
        directory = self.case / name
        directory.mkdir()
        (directory / "authorized_keys").write_text(
            self.key.with_suffix(".pub").read_text()
        )
        if format:
            (directory / "format").touch()
        if backend == "cas":
            wait_for(lambda: self.healthy() and Path(self.socket(image)).exists())
        else:
            raw = self.work / (image + ".raw")
            if format:
                with raw.open("xb") as file:
                    file.truncate(self.settings["image_bytes"])
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        env = {
            "CAS_RESULTS_DIR": str(directory),
            "CAS_SSH_PORT": str(port),
            "CAS_VHOST_SOCKET": self.socket(image),
            "CAS_RAW_IMAGE": str(self.work / (image + ".raw")),
            "TMPDIR": self.sockets.name,
        }
        process = self.spawn(
            name, [self.build["guests"][backend]], env, self.node.get("guest_cpus")
        )

        def ready():
            self.healthy()
            if process.poll() is not None:
                raise RuntimeError(f"guest {name} exited before ready")
            return (directory / "ready").exists() and (
                directory / "host-key.pub"
            ).exists()

        wait_for(ready, 600 if self.build["acceleration"] == "tcg" else 240)
        write_json(
            directory / "qemu.json",
            qemu_record(process.pid, self.build["acceleration"]),
        )
        known = directory / "known_hosts"
        known.write_text("guest " + (directory / "host-key.pub").read_text())
        control = Path(self.sockets.name) / f"{self.case.name}-{name}.control"
        self.guests[name] = [
            "ssh",
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=30",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            f"UserKnownHostsFile={known}",
            "-o",
            "HostKeyAlias=guest",
            "-i",
            str(self.key),
            "-p",
            str(port),
            "-S",
            str(control),
            "root@127.0.0.1",
        ]
        # A foreground, owned master avoids a new SSH handshake for each phase.
        # No ControlPersist daemon is left behind after the experiment.
        master = self.spawn(name + "-ssh", ["ssh", "-M", "-N", *self.guests[name][1:]])

        def connected():
            if master.poll() is not None:
                raise RuntimeError(f"guest SSH master exited: {name}")
            return control.exists()

        wait_for(connected)
        # Ready is a mounted filesystem, not an application readiness signal.
        wait_for(
            lambda: (
                subprocess.run(
                    [*self.guests[name], "true"],
                    check=False,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    timeout=120,
                ).returncode
                == 0
            )
        )
        return {
            "pid": process.pid,
            "port": port,
            "acceleration": self.build["acceleration"],
        }

    def shell(self, guest, script):
        return checked([*self.guests[guest], shlex.join(["sh", "-eu", "-c", script])])

    def fio(self, guest, kind, name, seconds=None, depth=1, seed=42):
        options = fio_options(
            kind,
            self.settings["data_bytes"],
            seconds or self.settings["seconds"],
            depth,
            seed,
        )
        if kind == "ship":
            options += ["--rate=" + str(self.settings["ship_rate_bytes"])]
        self.spawn(name, [*self.guests[guest], shlex.join(options)])
        return {"started_ns": time.monotonic_ns()}

    def drift(self, guest):
        # Fio verification headers contain write timestamps. Use identical
        # deterministic bytes here so independent updates really deduplicate.
        payload = random.Random(84).randbytes(self.settings["data_bytes"])
        digest = hashlib.sha256(payload).hexdigest()
        path = self.case / guest / "drift-payload"
        path.write_bytes(payload)
        start = time.monotonic_ns()
        output = self.shell(
            guest,
            "dd if=/results/drift-payload of=/mnt/cas/drift bs=4096 oflag=direct conv=fsync status=none; cd /mnt/cas; sha256sum drift; sync -f .",
        )
        if output.split()[0] != digest:
            raise ValueError("drift write verification failed")
        return {
            "bytes": len(payload),
            "sha256": digest,
            "elapsed_ns": time.monotonic_ns() - start,
        }

    def finish_fio(self, name):
        process = self.children.pop(name)
        try:
            status = process.wait(timeout=self.settings["seconds"] * 4 + 240)
        except BaseException:
            stop(process)
            raise
        if status != 0:
            raise RuntimeError(f"fio {name}: exit {status}; see {self.case}/{name}.log")
        result = json.loads((self.case / f"{name}.log").read_text())
        if not result["jobs"] or any(
            job["error"] or job["read"]["io_bytes"] + job["write"]["io_bytes"] == 0
            for job in result["jobs"]
        ):
            raise RuntimeError(f"fio verification/IO failed: {name}")
        write_json(self.case / f"{name}.json", result)
        return result

    def stop_guest(self, name):
        start = time.monotonic_ns()
        (self.case / name / "stop").touch()
        process = self.children[name]
        status = process.wait(timeout=180)
        if status:
            raise RuntimeError(f"guest shutdown failed: {name} {status}")
        del self.children[name]
        return {"shutdown_ns": time.monotonic_ns() - start}

    def settle(self):
        # Require a fresh complete telemetry sample after guest syncfs.
        journal = self.case / "daemon/telemetry.jsonl"
        time.sleep(0.6)

        def settled():
            self.healthy()
            if not journal.exists():
                return False
            lines = journal.read_text().splitlines()
            for line in reversed(lines):
                try:
                    value = json.loads(line)
                except json.JSONDecodeError:
                    continue
                states = [
                    entry["report"]["local"]["status"] for entry in value["images"]
                ]
                return (
                    value
                    if states
                    and all(
                        s["issued"] == s["published"] == s["compacted"] for s in states
                    )
                    else False
                )
            return False

        return wait_for(settled, 240)

    def capacity(self):
        total = 0
        groups = {}
        files = {}
        for path in self.work.rglob("*"):
            if path.is_file():
                size = path.stat().st_blocks * 512
                total += size
                files[str(path.relative_to(self.work))] = {
                    "allocated_bytes": size,
                    "logical_bytes": path.stat().st_size,
                }
                key = path.relative_to(self.work).parts[0]
                groups[key] = groups.get(key, 0) + size
        return {
            "allocated_bytes": total,
            "top_level_allocated_bytes": groups,
            "files": files,
            "filesystem": checked(["df", "-B1", str(self.work)]),
        }

    def raw_receiver(self):
        # Local fixture still crosses a real TCP socket for its baseline arm.
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        config = Path(self.sockets.name) / "rsync.conf"
        identity = "uid = 0\ngid = 0\n" if os.getuid() == 0 else ""
        config.write_text(
            f"use chroot = no\n{identity}[data]\npath = {self.work}\nread only = no\n"
        )
        self.spawn(
            "rsync-receiver",
            [
                "rsync",
                "--daemon",
                "--no-detach",
                "--address=127.0.0.1",
                f"--port={port}",
                f"--config={config}",
            ],
        )

        def ready():
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=1):
                    return True
            except OSError:
                return False

        wait_for(ready, 10)
        return f"rsync://127.0.0.1:{port}/data/"

    def rsync(self, destination, target, source=IMAGE):
        # A full-image arm: fresh target, whole-file, no compression, no sparse mode.
        remote = destination.get("ssh")
        path = str(Path(destination["work"]) / (target + ".raw"))
        dest = f"{remote}:{path}" if remote else destination["url"] + target + ".raw"
        argv = [
            "rsync",
            "--whole-file",
            "--ignore-times",
            "--no-compress",
            "--stats",
            "--fsync",
            "--protect-args",
            *(
                [
                    "-e",
                    "ssh -o BatchMode=yes -o StrictHostKeyChecking=yes",
                    "--rsync-path=" + destination.get("rsync", "rsync"),
                ]
                if remote
                else []
            ),
            "--",
            str(self.work / (source + ".raw")),
            dest,
        ]
        start = time.monotonic_ns()
        output = checked(argv, env={**os.environ, "LC_ALL": "C"})
        self.serial += 1
        (self.case / f"rsync-{self.serial}.log").write_text(output)
        from runner import rsync_bytes

        return {
            "elapsed_ns": time.monotonic_ns() - start,
            "stats": output,
            "application_bytes": rsync_bytes(output),
            "argv": argv,
        }

    def end_case(self):
        for process in reversed(list(self.children.values())):
            stop(process)
        self.children.clear()
        self.guests.clear()
        if self.work is not None:
            # Only this agent's freshly created case, never a caller-supplied existing root.
            shutil.rmtree(self.work)
            self.work = None
        return {"closed": True}

    def close(self):
        for process in reversed(list(self.children.values())):
            stop(process)
        self.children.clear()
        self.sockets.cleanup()

    def dispatch(self, request):
        allowed = {
            "configure",
            "start_case",
            "init",
            "host",
            "report",
            "fleet",
            "boot",
            "shell",
            "fio",
            "finish_fio",
            "drift",
            "stop_guest",
            "settle",
            "capacity",
            "rsync",
            "raw_receiver",
            "end_case",
        }
        if request["method"] not in allowed:
            raise ValueError("unknown agent operation")
        return getattr(self, request["method"])(**request.get("args", {}))
