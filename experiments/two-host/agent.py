"""Owned host-side operations for the two-host experiment (stdlib only)."""

import hashlib
import json
import os
import random
import resource
import secrets
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from contextlib import nullcontext
from pathlib import Path

STORE = "01" * 16
IMAGE = "02" * 16
EMPTY = "03" * 16
CLONE = "04" * 16
PAYLOAD_PIECE = 16 * 1024**2


def image_id(family, index):
    """Stable catalog IDs: index 0 keeps the legacy constant for its family."""
    if family not in (IMAGE, CLONE) or not 0 <= index < 256:
        raise ValueError("image family must be IMAGE or CLONE with index below 256")
    if index == 0:
        return family
    return family[:2] + f"{index:02x}" + "00" * 14


def sizing_args(guest, default):
    """Trailing QEMU arguments override the pinned run script's -m/-smp.

    The script's memfd backend keeps the build size, so a differently sized
    guest needs its own shared backend; QEMU takes the last -m/-smp/-machine.
    """
    cores, memory = int(guest["cores"]), int(guest["memory_mib"])
    if cores < 1 or memory < 128:
        raise ValueError("guest sizing needs at least one core and 128 MiB")
    if (cores, memory) == (int(default["cores"]), int(default["memory_mib"])):
        return []
    return [
        "-m",
        str(memory),
        "-smp",
        str(cores),
        "-object",
        f"memory-backend-memfd,id=cas-guest-mem,size={memory}M,share=on",
        "-machine",
        "memory-backend=cas-guest-mem",
    ]


def effective_sizing(argv):
    """Last -m/-smp values win in QEMU; read them from the live command line."""
    values = {}
    for flag, key in (("-m", "memory_mib"), ("-smp", "cores")):
        found = [argv[i + 1] for i, a in enumerate(argv[:-1]) if a == flag]
        if not found:
            raise ValueError(f"QEMU argv lacks {flag}")
        values[key] = int(found[-1].split(",")[0].removesuffix("M"))
    return values


def parse_block_stat(text):
    """/sys/block/<dev>/stat: read I/Os, merges, sectors, ticks, then writes."""
    fields = text.split()
    if len(fields) < 11:
        raise ValueError("unexpected block stat line")
    return {
        "read_ios": int(fields[0]),
        "read_sectors": int(fields[2]),
        "read_ticks_ms": int(fields[3]),
        "write_ios": int(fields[4]),
        "write_sectors": int(fields[6]),
    }


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + "\n")


def checked(argv, timeout=600, **kwargs):
    result = subprocess.run(
        argv, check=False, text=True, capture_output=True, timeout=timeout, **kwargs
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


def ssh_ready(port):
    # Disk readiness and sshd's host key precede late interface configuration.
    # Probe the forwarded path; the authenticated master still checks the key.
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=30) as stream:
            return stream.recv(128).startswith(b"SSH-2.0-")
    except OSError:
        return False


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


def qemu_record(pid, acceleration, sizing=None):
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
                effective = effective_sizing(argv)
                if sizing and effective != {
                    "cores": int(sizing["cores"]),
                    "memory_mib": int(sizing["memory_mib"]),
                }:
                    raise ValueError(
                        f"QEMU runs {effective}, not the configured guest sizing {sizing}"
                    )
                return {
                    "pid": current,
                    "executable": executable,
                    "argv": argv,
                    "kvm_vm_fd": kvm,
                    "sizing": effective,
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
        self.zfs_dataset = None
        self.lock = threading.Lock()
        self.launches = {}

    def spawn(self, name, argv, env=None, pin=None, pass_fds=(), separate_stderr=False):
        if pin:
            argv = ["taskset", "-c", pin, *argv]
        with self.lock:
            if name in self.children:
                raise ValueError(f"child already exists: {name}")
            write_json(self.case / f"{name}-command.json", {"argv": argv, "env": env})
            with (
                (self.case / f"{name}.log").open("x") as log,
                (
                    (self.case / f"{name}.stderr.log").open("x")
                    if separate_stderr
                    else nullcontext(log)
                ) as errors,
            ):
                process = subprocess.Popen(
                    argv,
                    stdin=subprocess.DEVNULL,
                    stdout=log,
                    stderr=errors,
                    env={**os.environ, **(env or {})},
                    start_new_session=True,
                    pass_fds=pass_fds,
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
        self.host_generation = 0
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

    def init(self, node, images=None):
        root = self.work / "store"
        root.mkdir()
        if images is None:
            images = [IMAGE] if node == 0 else [(128 + node).to_bytes(16, "big").hex()]
        if not images:
            raise ValueError("initialization needs at least one image")
        argv = [self.build["host"], "init", *self.storage_args()]
        for image in images:
            argv += ["--image", f"{image}={self.settings['image_bytes']}"]
        return json.loads(checked(argv))

    def host(self, node, k, members, cache, force=False, images=None):
        self.host_options = {
            "node": node,
            "k": k,
            "members": members,
            "cache": cache,
            "force": force,
            "images": images,
        }
        reports = self.case / (
            "daemon" if self.host_generation == 0 else f"daemon-{self.host_generation}"
        )
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
                "--peer-bulk-listen",
                self.node["bulk"],
                "--peer-inline-us",
                str(self.settings["inline_us"]),
            ]
            for member in members:
                argv += [
                    "--peer-get-member",
                    member["get"],
                    "--peer-bulk-member",
                    member["bulk"],
                ]
        if force:
            argv.append("--peer-force-remote")
        for image in ((IMAGE, CLONE) if k else (IMAGE,)) if images is None else images:
            argv += ["--image", f"{image}={self.socket(image)}"]
        self.spawn("host", argv, pin=self.node.get("cpus"))
        return {"pid": self.children["host"].pid}

    def restart_host(self, images=None):
        # Guest shutdown consumes this daemon's one-shot image attachment.
        # Cold recovery reconstructs it before the fenced handoff. The clean
        # chunk cache is memory only, so a restart always starts it empty.
        if any(name in self.children for name in self.guests):
            raise ValueError("stop every guest before restarting its daemon")
        stop(self.children.pop("host"))
        for sock in Path(self.sockets.name).glob(f"{self.case.name}-*.sock"):
            sock.unlink(missing_ok=True)
        for suffix in ("log", "command.json"):
            source = (
                self.case / f"host.{suffix}"
                if suffix == "log"
                else self.case / "host-command.json"
            )
            source.rename(self.case / f"host-{self.host_generation}-{suffix}")
        self.host_generation += 1
        if images is None:
            images = [IMAGE] if self.host_options["node"] == 1 else []
        started = time.monotonic_ns()
        result = self.host(**{**self.host_options, "images": images})
        return {
            **result,
            "restarted_ns": started,
            "cache_bytes": self.host_options["cache"],
        }

    def drop_page_cache(self):
        # Only when the configuration asks; never silently skipped or faked.
        path = Path("/proc/sys/vm/drop_caches")
        if os.geteuid() != 0 or not os.access(path, os.W_OK):
            raise PermissionError(
                "dropping the host page cache needs root; run without drop_page_cache"
            )
        os.sync()
        path.write_text("3\n")
        return {"dropped": True, "unix_ns": time.time_ns()}

    def socket(self, image):
        return str(Path(self.sockets.name) / f"{self.case.name}-{image}.sock")

    def report(self):
        self.healthy()
        return self.fleet(["report", "--address", self.node["bulk"]])["result"]

    def fleet(self, argv):
        return json.loads(checked([self.build["fleet"], *argv]))

    def boot(self, name, image=IMAGE, backend="cas", format=False, sizing=None):
        launched = self.launch(name, image, backend, format, sizing)
        return {**launched, **self.await_guest(name)}

    def launch(self, name, image=IMAGE, backend="cas", format=False, sizing=None):
        """Start QEMU and return immediately; await_guest completes readiness."""
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
        default = self.build.get("guest_sizing", {"cores": 2, "memory_mib": 1024})
        sizing = sizing or default
        launched_ns = time.monotonic_ns()
        process = self.spawn(
            name,
            [self.build["guests"][backend], *sizing_args(sizing, default)],
            env,
            self.node.get("guest_cpus"),
        )
        self.launches[name] = {
            "process": process,
            "port": port,
            "directory": directory,
            "sizing": sizing,
            "launched_ns": launched_ns,
        }
        return {"pid": process.pid, "port": port, "launched_ns": launched_ns}

    def await_guest(self, name):
        launch = self.launches.pop(name)
        process, port, directory = (
            launch["process"],
            launch["port"],
            launch["directory"],
        )
        timings = {"launched_ns": launch["launched_ns"]}

        def ready():
            self.healthy()
            if process.poll() is not None:
                raise RuntimeError(f"guest {name} exited before ready")
            return (directory / "ready").exists() and (
                directory / "host-key.pub"
            ).exists()

        wait_for(ready, 1200 if self.build["acceleration"] == "tcg" else 240)
        timings["ready_ns"] = time.monotonic_ns()
        write_json(
            directory / "qemu.json",
            qemu_record(process.pid, self.build["acceleration"], launch["sizing"]),
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
            "ConnectTimeout=120",
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
        wait_for(lambda: ready() and ssh_ready(port), 180)
        timings["ssh_banner_ns"] = time.monotonic_ns()
        # The first command runs without the master so its timestamp does not
        # include master setup. Ready is a mounted filesystem, not an
        # application readiness signal.
        direct = [a for a in self.guests[name] if a != "-S" and a != str(control)]
        wait_for(
            lambda: (
                subprocess.run(
                    [*direct, "true"],
                    check=False,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    timeout=120,
                ).returncode
                == 0
            )
        )
        timings["first_command_ns"] = time.monotonic_ns()
        # A foreground, owned master avoids a new SSH handshake for each phase.
        # No ControlPersist daemon is left behind after the experiment.
        master = self.spawn(name + "-ssh", ["ssh", "-M", "-N", *self.guests[name][1:]])

        def connected():
            if master.poll() is not None:
                raise RuntimeError(f"guest SSH master exited: {name}")
            return control.exists()

        wait_for(connected)
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
            "sizing": launch["sizing"],
            **timings,
        }

    def clock(self):
        """This host's monotonic clock, for anchors other hosts cannot supply."""
        return {"monotonic_ns": time.monotonic_ns()}

    def boot_burst(self, guests, probe=None, sample_ring=False):
        """Launch every guest back to back, then observe each one concurrently.

        `probe` is a shell script run immediately after the first successful
        command; its output and completion time are recorded per guest. With
        `sample_ring` the daemon report (and so its bounded remote-read ring)
        is taken right after the probe, before later reads can evict the
        earliest post-boot records.
        """
        if not guests:
            raise ValueError("burst needs at least one guest")
        launches = [self.launch(**guest) for guest in guests]

        def observe(guest):
            result = self.await_guest(guest["name"])
            if probe:
                result["probe"] = self.shell(guest["name"], probe)
                result["probe_ns"] = time.monotonic_ns()
            if sample_ring:
                result["report_at_ready"] = self.report()
                result["report_at_ready_ns"] = time.monotonic_ns()
            return result

        with ThreadPoolExecutor(max_workers=len(guests)) as executor:
            observed = list(executor.map(observe, guests))
        return [
            {
                "name": guest["name"],
                "image": guest.get("image", IMAGE),
                **launch,
                **seen,
            }
            for guest, launch, seen in zip(guests, launches, observed, strict=True)
        ]

    def shell(self, guest, script, timeout=600):
        return checked(
            [*self.guests[guest], shlex.join(["sh", "-eu", "-c", script])],
            timeout=timeout,
        )

    def run_commands(self, guests, commands, label, timeout=1800):
        """Replay a recorded command list in every guest at once.

        Each guest runs its list in order from the data disk; a failing command
        fails the case. Completion times are this host's monotonic clock, the
        same clock as the guest launch milestones. Stdout is retained per
        command under the guest's directory, not in the RPC reply.
        """
        if not guests or not commands:
            raise ValueError("working set needs guests and commands")
        if not label.replace("-", "").isalnum():
            raise ValueError("invalid working-set label")

        def replay(guest):
            rows = []
            for index, command in enumerate(commands):
                output = self.case / guest / f"working-set-{label}-{index}.out"
                started = time.monotonic_ns()
                text = self.shell(guest, "cd /mnt/cas\n" + command, timeout)
                completed = time.monotonic_ns()
                output.write_text(text)
                rows.append(
                    {
                        "index": index,
                        "command": command,
                        "started_ns": started,
                        "completed_ns": completed,
                        "elapsed_ns": completed - started,
                        "stdout_bytes": len(text.encode()),
                        "stdout_sha256": hashlib.sha256(text.encode()).hexdigest(),
                    }
                )
            return {"name": guest, "commands": rows}

        with ThreadPoolExecutor(max_workers=len(guests)) as executor:
            return list(executor.map(replay, guests))

    def calibrate_ring(self, guest, path, reads=32, chunk_bytes=16384):
        """Bracket the daemon's ring clock against this host's monotonic clock.

        The peer ring stamps reads in nanoseconds since the peer runtime
        started, so its records are not directly comparable to launch or
        command timestamps. Each direct 4 KiB read of a distinct chunk is
        bracketed on the host clock; a read that adds exactly one ring record
        bounds the ring origin to [started - received, completed - completed].
        Reads of locally owned chunks add no record and are retained as such.
        """
        if reads < 1 or chunk_bytes % 4096:
            raise ValueError("calibration needs reads and a 4 KiB chunk multiple")
        if "/" not in path or any(c.isspace() for c in path):
            raise ValueError("calibration path must be an absolute guest path")
        rows = []
        previous = self.report()["peer"]
        for index in range(reads):
            skip = index * chunk_bytes // 4096
            started = time.monotonic_ns()
            self.shell(
                guest,
                f"dd if={path} bs=4096 count=1 skip={skip} iflag=direct status=none > /dev/null",
            )
            completed = time.monotonic_ns()
            current = self.report()["peer"]
            known = {record["id"] for record in previous["recent"]}
            rows.append(
                {
                    "index": index,
                    "offset": skip * 4096,
                    "started_ns": started,
                    "completed_ns": completed,
                    "completed_reads": current["counters"]["completed_reads"]
                    - previous["counters"]["completed_reads"],
                    "records": [
                        record
                        for record in current["recent"]
                        if record["id"] not in known
                    ],
                }
            )
            previous = current
        return {"guest": guest, "path": path, "chunk_bytes": chunk_bytes, "reads": rows}

    def fio(self, guest, kind, name, seconds=None, depth=1, seed=42, rate=None):
        options = fio_options(
            kind,
            self.settings["data_bytes"],
            seconds or self.settings["seconds"],
            depth,
            seed,
        )
        if kind == "ship":
            options += ["--rate=" + str(rate or self.settings["ship_rate_bytes"])]
        self.spawn(
            name, [*self.guests[guest], shlex.join(options)], separate_stderr=True
        )
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

    def write_payload(self, guest, name, seed, size, marker=None, block_size=1024**2):
        """Deterministic seeded bytes through the guest's direct write path.

        Identical seeds produce identical content across guests, so shared
        regions really deduplicate. An optional marker replaces the first
        4 KiB block with recognizable text for the boot probe.
        """
        if size <= 0 or size % 4096 or block_size % 4096:
            raise ValueError("payload and block sizes must be positive 4 KiB multiples")
        shared = self.case / f"payload-{seed}-{size}-{marker or 'plain'}"
        if not shared.exists():
            generator = random.Random(seed)
            with shared.open("xb") as file:
                remaining = size
                first = True
                while remaining:
                    piece = generator.randbytes(min(PAYLOAD_PIECE, remaining))
                    if first and marker:
                        piece = marker.encode().ljust(4096, b"\0") + piece[4096:]
                    first = False
                    file.write(piece)
                    remaining -= len(piece)
        with shared.open("rb") as file:
            digest = hashlib.file_digest(file, "sha256").hexdigest()
        link = self.case / guest / f"{name}-payload"
        if not link.exists():
            os.link(shared, link)
        start = time.monotonic_ns()
        output = self.shell(
            guest,
            f"dd if=/results/{name}-payload of=/mnt/cas/{name} bs={block_size} oflag=direct conv=fsync status=none; cd /mnt/cas; sha256sum {name}; sync -f .",
        )
        if output.split()[0] != digest:
            raise ValueError(f"payload write verification failed: {name}")
        return {
            "name": name,
            "bytes": size,
            "sha256": digest,
            "seed": seed,
            "marker": marker,
            "elapsed_ns": time.monotonic_ns() - start,
        }

    def guest_stat(self, guest):
        """Guest-observed device reads and ext4 occupancy; no host attribution."""
        output = self.shell(
            guest,
            "cat /sys/block/vda/stat; stat -f -c '%b %f %S' /mnt/cas",
        ).splitlines()
        blocks, free, fragment = (int(v) for v in output[-1].split())
        return {
            "device": parse_block_stat(output[0]),
            "used_bytes": (blocks - free) * fragment,
            "sampled_ns": time.monotonic_ns(),
        }

    def fio_running(self, name):
        self.healthy()
        return self.children[name].poll() is None

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
        with self.lock:
            del self.children[name]
        return {"shutdown_ns": time.monotonic_ns() - start}

    def stop_guests(self, names):
        with ThreadPoolExecutor(max_workers=max(1, len(names))) as executor:
            return list(executor.map(self.stop_guest, names))

    def settle(self):
        # Require a fresh complete telemetry sample after guest syncfs.
        generation = getattr(self, "host_generation", 0)
        journal = (
            self.case
            / ("daemon" if generation == 0 else f"daemon-{generation}")
            / "telemetry.jsonl"
        )
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
                    if all(
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

    def zfs_prepare(self, create=False):
        parent = self.node.get("zfs_parent")
        if not parent or any(c in parent for c in "@#\n\r"):
            raise ValueError("zfs-incremental needs an existing dedicated zfs_parent")
        if checked(["zfs", "list", "-H", "-o", "type", parent]).strip() != "filesystem":
            raise ValueError("zfs_parent must be a filesystem")
        dataset = parent + "/" + self.storage.name + "-" + self.case.name
        exists = subprocess.run(
            ["zfs", "list", "-H", "-o", "name", dataset],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        if exists.returncode == 0:
            raise ValueError("ZFS experiment dataset must be new")
        self.zfs_dataset = dataset
        if create:
            checked(
                [
                    "zfs",
                    "create",
                    "-o",
                    "mountpoint=" + str(self.work),
                    "-o",
                    "compression=off",
                    "-o",
                    "atime=off",
                    "-o",
                    "recordsize=4K",
                    dataset,
                ]
            )
        return {
            "dataset": dataset,
            "mountpoint": str(self.work),
            "version": checked(["zfs", "version"]),
        }

    def zfs_snapshot(self, name, create=True):
        if not name.isalnum():
            raise ValueError("invalid snapshot name")
        snapshot = self.zfs_dataset + "@" + name
        if create:
            checked(["zfs", "snapshot", snapshot])
        return {
            "name": name,
            "snapshot": snapshot,
            "guid": checked(
                ["zfs", "get", "-Hp", "-o", "value", "guid", snapshot]
            ).strip(),
        }

    def zfs_receiver(self, base=None, guid=None):
        if any(name in self.children for name in self.guests):
            raise ValueError("stop destination guests before receiving disk state")
        if base:
            if not base.isalnum() or not guid:
                raise ValueError("incremental receive needs a common-base GUID")
            actual = checked(
                [
                    "zfs",
                    "get",
                    "-Hp",
                    "-o",
                    "value",
                    "guid",
                    self.zfs_dataset + "@" + base,
                ]
            ).strip()
            if actual != guid:
                raise ValueError("ZFS incremental base GUID differs")
        host = self.node["bulk"].rsplit(":", 1)[0].strip("[]")
        bind = self.node.get("zfs_bind", host)
        listener = socket.socket(socket.AF_INET6 if ":" in bind else socket.AF_INET)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind((bind, self.node.get("zfs_port", 0)))
        listener.listen(1)
        port = listener.getsockname()[1]
        token = secrets.token_hex(32)
        self.serial += 1
        name = f"zfs-receive-{self.serial}"
        result = self.case / (name + ".json")
        argv = [
            sys.executable,
            str(Path(__file__).with_name("zfs_stream.py")),
            "--mode",
            "receive",
            "--fd",
            str(listener.fileno()),
            "--token",
            token,
            "--result",
            str(result),
            "--",
            "zfs",
            "receive",
            "-u",
            *(["-F"] if base else []),
            "-o",
            "recordsize=4K",
            "-o",
            "compression=off",
            "-o",
            "atime=off",
            "-o",
            "mountpoint=" + str(self.work),
            self.zfs_dataset,
        ]
        try:
            self.spawn(name, argv, pass_fds=(listener.fileno(),))
        finally:
            listener.close()
        return {
            "name": name,
            "host": host,
            "port": port,
            "token": token,
            "base_guid": guid,
        }

    def zfs_send(self, destination, snapshot, base=None):
        if not snapshot.isalnum() or base and not base.isalnum():
            raise ValueError("invalid snapshot name")
        self.serial += 1
        name = f"zfs-send-{self.serial}"
        result = self.case / (name + ".json")
        argv = [
            sys.executable,
            str(Path(__file__).with_name("zfs_stream.py")),
            "--mode",
            "send",
            "--host",
            destination["host"],
            "--port",
            str(destination["port"]),
            "--token",
            destination["token"],
            "--result",
            str(result),
            "--",
            "zfs",
            "send",
            *(["-i", self.zfs_dataset + "@" + base] if base else []),
            self.zfs_dataset + "@" + snapshot,
        ]
        process = self.spawn(name, argv)
        try:
            if process.wait(timeout=900):
                raise RuntimeError("ZFS send failed; inspect sender and receiver logs")
        finally:
            stop(process)
            del self.children[name]
        return json.loads(result.read_text())

    def zfs_finish_receive(self, name):
        if not name.startswith("zfs-receive-"):
            raise ValueError("invalid owned receiver")
        process = self.children[name]
        try:
            if process.wait(timeout=900):
                raise RuntimeError("ZFS receive failed")
        finally:
            stop(process)
            del self.children[name]
        if (
            checked(
                ["zfs", "get", "-Hp", "-o", "value", "mounted", self.zfs_dataset]
            ).strip()
            == "no"
        ):
            checked(["zfs", "mount", self.zfs_dataset])
        return json.loads((self.case / (name + ".json")).read_text())

    def zfs_capacity(self):
        return {
            "dataset": self.zfs_dataset,
            "properties": checked(
                [
                    "zfs",
                    "get",
                    "-Hp",
                    "-r",
                    "used,referenced,logicalused,usedbysnapshots,compressratio,recordsize,compression,guid",
                    self.zfs_dataset,
                ]
            ),
        }

    def end_case(self):
        for process in reversed(list(self.children.values())):
            stop(process)
        self.children.clear()
        self.guests.clear()
        if self.zfs_dataset is not None:
            checked(["zfs", "destroy", "-r", self.zfs_dataset])
            self.zfs_dataset = None
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
            "zfs_prepare",
            "zfs_snapshot",
            "zfs_receiver",
            "zfs_send",
            "zfs_finish_receive",
            "zfs_capacity",
            "configure",
            "start_case",
            "init",
            "host",
            "report",
            "restart_host",
            "drop_page_cache",
            "fleet",
            "boot",
            "launch",
            "await_guest",
            "boot_burst",
            "clock",
            "run_commands",
            "calibrate_ring",
            "shell",
            "fio",
            "finish_fio",
            "fio_running",
            "drift",
            "write_payload",
            "guest_stat",
            "stop_guest",
            "stop_guests",
            "settle",
            "capacity",
            "rsync",
            "raw_receiver",
            "end_case",
        }
        if request["method"] not in allowed:
            raise ValueError("unknown agent operation")
        return getattr(self, request["method"])(**request.get("args", {}))
