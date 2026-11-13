"""Pure logic of the CloudLab campaign driver: plan loading, arm validation,
pinning checks, unit scheduling and resume decisions. No subprocesses, no
network. See docs/cloudlab-campaign.md."""

import hashlib
import json
import re
import statistics
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
ARMS_DIR = HERE / "arms"
RUNNERS = ("two-host", "native", "r0")
NATIVE_BACKENDS = ("raw", "daemon", "cas")
NATIVE_MAX_GUESTS = 2  # cas-harness native: --guests 1..=2
MAX_GUESTS = 16
PLACEHOLDERS = ("RACE_BUDGET_US", "WORKING_SET_FROM_BUNDLE")
# Settings the two-host runner must know for an arm to be measurable; a runner
# without them would silently ignore the flag and mislabel the arm.
RUNNER_SETTINGS = ("fast_settled_reads", "peer_race_us")
UNIT_ID = re.compile(r"^d(\d+)-([a-z0-9-]+)-([a-z0-9-]+)-r(\d+)$")
STATES = ("absent", "done", "failed", "partial")
WORKING_SET_COMMANDS = 10


@dataclass(frozen=True)
class Unit:
    arm: str
    case: str
    repetitions: int


@dataclass
class Day:
    day: int
    units: list = field(default_factory=list)


def parse_cpuset(text):
    """`2-5,8` to a sorted list of distinct CPU ids; rejects anything else."""
    if not isinstance(text, str) or not text.strip():
        raise ValueError(f"cpu set must be a non-empty string: {text!r}")
    cpus = set()
    for part in text.split(","):
        match = re.fullmatch(r"\s*(\d+)(?:-(\d+))?\s*", part)
        if not match:
            raise ValueError(f"bad cpu set {text!r}")
        low = int(match[1])
        high = int(match[2]) if match[2] else low
        if high < low:
            raise ValueError(f"descending range in cpu set {text!r}")
        cpus.update(range(low, high + 1))
    return sorted(cpus)


def format_cpuset(cpus):
    cpus = sorted(set(cpus))
    ranges, start = [], None
    for index, cpu in enumerate(cpus):
        if start is None:
            start = cpu
        last = index + 1 == len(cpus) or cpus[index + 1] != cpu + 1
        if last:
            ranges.append(str(start) if start == cpu else f"{start}-{cpu}")
            start = None
    return ",".join(ranges)


# ---------------------------------------------------------------- day plan


def load_plan(value):
    """A plan is `{"days": [{"day": 1, "units": [...]}, ...]}` or a bare list
    of units (one day). A unit is `[arm, case, repetitions]` or an object with
    those keys. Day numbers increase (a repeats plan starts at day 4);
    repetitions 1..20."""
    if isinstance(value, (str, Path)):
        value = json.loads(Path(value).read_text())
    days = value.get("days") if isinstance(value, dict) else [{"units": value}]
    if not isinstance(days, list) or not days:
        raise ValueError("plan needs a non-empty list of days")
    plan = []
    previous = 0
    for index, day in enumerate(days, start=1):
        if not isinstance(day, dict) or not isinstance(day.get("units"), list):
            raise ValueError(f"day {index} needs a units list")
        number = int(day.get("day", previous + 1))
        if number <= previous:
            raise ValueError(f"days must be numbered in increasing order; got {number} after {previous}")
        previous = number
        units = [parse_unit(unit) for unit in day["units"]]
        if not units:
            raise ValueError(f"day {number} has no units")
        if len({(u.arm, u.case) for u in units}) != len(units):
            raise ValueError(f"day {number} lists an (arm, case) twice")
        plan.append(Day(number, units))
    return plan


def parse_unit(unit):
    if isinstance(unit, (list, tuple)):
        if len(unit) != 3:
            raise ValueError(f"unit needs [arm, case, repetitions]: {unit!r}")
        arm, case, reps = unit
    elif isinstance(unit, dict) and set(unit) == {"arm", "case", "repetitions"}:
        arm, case, reps = unit["arm"], unit["case"], unit["repetitions"]
    else:
        raise ValueError(f"unit needs arm, case and repetitions: {unit!r}")
    for name in (arm, case):
        if not isinstance(name, str) or not re.fullmatch(r"[a-z0-9][a-z0-9-]*", name):
            raise ValueError(f"arm and case names are lowercase slugs: {unit!r}")
    if not isinstance(reps, int) or isinstance(reps, bool) or not 1 <= reps <= 20:
        raise ValueError(f"repetitions must be an integer from 1 to 20: {unit!r}")
    return Unit(arm, case, reps)


def unit_id(day, unit, rep):
    return f"d{day}-{unit.arm}-{unit.case}-r{rep}"


def rotate(items, shift):
    if not items:
        return []
    shift %= len(items)
    return list(items[shift:]) + list(items[:shift])


def schedule(plan):
    """Ordered (day, unit, rep) rows. Repetition is the outer loop within a
    day and the arm order is rotated by the repetition index, so no arm always
    runs first or last; units with fewer repetitions drop out of later rounds."""
    rows = []
    for day in plan:
        rounds = max(u.repetitions for u in day.units)
        for rep in range(1, rounds + 1):
            for unit in rotate(day.units, rep - 1):
                if rep <= unit.repetitions:
                    rows.append((day.day, unit, rep))
    return rows


# ---------------------------------------------------------------- arms


def load_arms(directory=ARMS_DIR):
    arms = {}
    for path in sorted(Path(directory).glob("*.json")):
        arm = json.loads(path.read_text())
        validate_arm(arm)
        if arm["arm"] != path.stem:
            raise ValueError(f"{path.name}: arm name {arm['arm']!r} differs from file")
        arm["_path"] = str(path)
        arm["_sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
        arms[arm["arm"]] = arm
    if not arms:
        raise ValueError(f"no arm configs under {directory}")
    return arms


def validate_arm(arm):
    if not isinstance(arm, dict):
        raise ValueError("arm config must be an object")
    for key in ("arm", "runner", "description", "pinning"):
        if key not in arm:
            raise ValueError(f"arm config needs {key!r}")
    if arm["runner"] not in RUNNERS:
        raise ValueError(f"{arm['arm']}: runner must be one of {RUNNERS}")
    validate_pinning(arm["pinning"], arm["arm"])
    if arm["runner"] == "two-host":
        validate_two_host_arm(arm)
    elif arm["runner"] == "native":
        validate_native_arm(arm)
    elif arm["runner"] == "r0":
        if arm.get("cases") != ["floor"]:
            raise ValueError("the r0 arm has exactly one case, floor")


def validate_pinning(pinning, arm):
    if not isinstance(pinning, dict) or not pinning:
        raise ValueError(f"{arm}: pinning needs one entry per node type")
    for node_type, sets in pinning.items():
        if not isinstance(sets, dict):
            raise ValueError(f"{arm}: pinning[{node_type}] must be an object")
        for key in ("logical_cpus", "irq", "daemon", "controller", "guests"):
            if key not in sets:
                raise ValueError(f"{arm}: pinning[{node_type}] needs {key!r}")
        if not isinstance(sets["logical_cpus"], int) or sets["logical_cpus"] < 2:
            raise ValueError(f"{arm}: pinning[{node_type}].logical_cpus must be >= 2")
        for key in ("irq", "daemon", "controller", "guests"):
            for cpuset in member_sets(sets[key]):
                parse_cpuset(cpuset)


def member_sets(value):
    """A pinning entry is one cpu set for every member, or a list with one
    set per member (loopback layouts share a host and need disjoint sets)."""
    return list(value) if isinstance(value, list) else [value]


def pin_for(sets, key, member):
    values = member_sets(sets[key])
    return values[member] if len(values) > 1 else values[0]


def validate_two_host_arm(arm):
    config = arm.get("config")
    if not isinstance(config, dict) or "settings" not in config:
        raise ValueError(f"{arm['arm']}: two-host arms need config.settings")
    cases = config.get("cases")
    if not isinstance(cases, list) or not cases or not all(isinstance(c, str) for c in cases):
        raise ValueError(f"{arm['arm']}: config.cases must list the runnable cases")
    for case in cases:
        if case_guests(arm, case) is None:
            raise ValueError(f"{arm['arm']}: unknown two-host case {case!r}")
    settings = config["settings"]
    if "guest" in settings:
        guest = settings["guest"]
        if set(guest) != {"cores", "memory_mib"} or guest["cores"] < 1:
            raise ValueError(f"{arm['arm']}: settings.guest needs cores and memory_mib")
    requires = arm.get("requires_runner_settings", [])
    if any(r not in RUNNER_SETTINGS for r in requires):
        raise ValueError(f"{arm['arm']}: requires_runner_settings must be from {RUNNER_SETTINGS}")
    for key in requires:
        if key not in settings:
            raise ValueError(f"{arm['arm']}: requires {key} but settings lacks it")


def validate_native_arm(arm):
    if arm.get("backend") not in NATIVE_BACKENDS:
        raise ValueError(f"{arm['arm']}: native backend must be one of {NATIVE_BACKENDS}")
    if arm.get("filesystem", "xfs") not in ("xfs", "zfs"):
        raise ValueError(f"{arm['arm']}: filesystem must be xfs or zfs")
    if arm.get("filesystem", "xfs") == "zfs" and arm["backend"] != "raw":
        raise ValueError(f"{arm['arm']}: only the raw backend may store on zfs")
    cases = arm.get("cases")
    if not isinstance(cases, dict) or not cases:
        raise ValueError(f"{arm['arm']}: native arms map case names to guests/script_args")
    for name, case in cases.items():
        if not isinstance(case, dict) or not isinstance(case.get("guests"), int):
            raise ValueError(f"{arm['arm']}: case {name!r} needs an integer guests")
        if not 1 <= case["guests"] <= MAX_GUESTS:
            raise ValueError(f"{arm['arm']}: case {name!r} guests must be 1..{MAX_GUESTS}")
        args = case.get("script_args", [])
        if not isinstance(args, list) or not all(isinstance(a, str) for a in args):
            raise ValueError(f"{arm['arm']}: case {name!r} script_args must be strings")


def case_guests(arm, case):
    """Concurrent workload guests per host that a case boots, or None if the
    case name is not one the arm's runner knows."""
    if arm["runner"] == "native":
        entry = arm["cases"].get(case)
        return entry["guests"] if entry else None
    if arm["runner"] == "r0":
        return 0 if case == "floor" else None
    settings = arm["config"]["settings"]
    match = re.fullmatch(r"reads-(memory|storage)(?:-g(\d+))?", case)
    if match:
        return int(match[2]) if match[2] else 1
    if case == "burst-images":
        return int(settings.get("burst", {}).get("images", 4))
    if case in ("flows-k1", "flows-k2", "raw-rsync", "zfs-incremental", "independent-stores", "resume-elsewhere"):
        return 1
    return None


def arm_cases(arm):
    if arm["runner"] == "native":
        return list(arm["cases"])
    if arm["runner"] == "r0":
        return ["floor"]
    return list(arm["config"]["cases"])


def validate_plan(plan, arms):
    """Every planned (arm, case) must exist in the arm configs."""
    problems = []
    for day in plan:
        for unit in day.units:
            arm = arms.get(unit.arm)
            if arm is None:
                problems.append(f"day {day.day}: unknown arm {unit.arm!r}")
            elif unit.case not in arm_cases(arm):
                problems.append(f"day {day.day}: arm {unit.arm!r} has no case {unit.case!r}; it has {arm_cases(arm)}")
    return problems


# ---------------------------------------------------------------- pinning


def check_pinning(sets, member, ncpu, guest_cores, guests):
    """Problems with one member's pinning on a host with `ncpu` logical CPUs
    for a case that boots `guests` guests of `guest_cores` vCPUs. The host
    budget rule is the two-host doc's: cores × N guest threads, daemon CPUs
    disjoint from them, plus two threads for the controller/agent."""
    problems = []
    chosen = {}
    for key in ("irq", "daemon", "controller", "guests"):
        chosen[key] = parse_cpuset(pin_for(sets, key, member))
        if chosen[key][-1] >= ncpu:
            problems.append(f"{key} set {format_cpuset(chosen[key])} exceeds the {ncpu} logical cpus")
    if ncpu != sets["logical_cpus"]:
        problems.append(f"pinning assumes {sets['logical_cpus']} logical cpus, inventory has {ncpu}")
    if set(chosen["daemon"]) & set(chosen["guests"]):
        problems.append("daemon and guest sets overlap")
    if set(chosen["irq"]) & set(chosen["guests"]):
        problems.append("irq and guest sets overlap")
    need = guest_cores * guests
    if need and len(chosen["guests"]) < need:
        problems.append(f"{guests} guests x {guest_cores} vCPU need {need} guest cpus, set has {len(chosen['guests'])}")
    if len(chosen["controller"]) < 2:
        problems.append("controller set needs at least two cpus")
    total = need + len(chosen["daemon"]) + 2
    if total > ncpu:
        problems.append(f"budget {need} guest + {len(chosen['daemon'])} daemon + 2 = {total} exceeds {ncpu} cpus")
    return problems


def guest_cores(arm, case):
    if arm["runner"] != "two-host":
        return 2 if arm["runner"] == "native" else 0
    settings = arm["config"]["settings"]
    block = settings.get("burst" if case == "burst-images" else "resume" if case == "resume-elsewhere" else "", {})
    return int((block.get("guest") or settings.get("guest") or {"cores": 2})["cores"])


# ---------------------------------------------------------------- placeholders


def substitute(value, fills):
    """Replace placeholder strings anywhere in a JSON value; a placeholder
    that stands alone is replaced by the fill (which may be a list or an
    object). An object with a `bundle` key whose fill is an object is merged:
    the bundle's commands, cwd and env, overridden by the object's own keys
    (so an arm can add `chroot` to a bundle's working set)."""
    if isinstance(value, str) and value in fills:
        return fills[value]
    if isinstance(value, dict):
        result = {k: substitute(v, fills) for k, v in value.items() if k != "bundle"}
        if "bundle" in value:
            bundle = substitute(value["bundle"], fills)
            if isinstance(bundle, dict):
                return {**{k: bundle[k] for k in ("commands", "cwd", "env") if k in bundle}, **result}
            result["bundle"] = bundle
        return result
    if isinstance(value, list):
        return [substitute(v, fills) for v in value]
    return value


def unresolved(value):
    if isinstance(value, str):
        return [value] if value in PLACEHOLDERS else []
    if isinstance(value, dict):
        return [p for v in value.values() for p in unresolved(v)]
    if isinstance(value, list):
        return [p for v in value for p in unresolved(v)]
    return []


def working_set_from_bundle(actions_path, count=WORKING_SET_COMMANDS):
    """The first `count` recorded bash actions of a corpus bundle, verbatim,
    with the recorded cwd and environment. `actions.json` is the swe
    bundle's object (`cwd`, `env`, `interpreter`, `timeout`, `steps` of
    `{"command": ...}`); a bare list of such rows is accepted without cwd
    or env."""
    path = Path(actions_path)
    data = path.read_bytes()
    actions = json.loads(data)
    if isinstance(actions, list):
        actions = {"steps": actions}
    if not isinstance(actions, dict) or not isinstance(actions.get("steps"), list) or not actions["steps"]:
        raise ValueError(f"{path}: actions.json needs a non-empty steps list")
    commands = []
    for row in actions["steps"][:count]:
        command = row.get("command") if isinstance(row, dict) else None
        if not isinstance(command, str) or not command.strip():
            raise ValueError(f"{path}: action without a command string")
        commands.append(command)
    cwd, env = actions.get("cwd"), actions.get("env") or {}
    if cwd is not None and not (isinstance(cwd, str) and cwd.startswith("/")):
        raise ValueError(f"{path}: recorded cwd must be an absolute path")
    if not isinstance(env, dict) or not all(isinstance(k, str) and isinstance(v, str) for k, v in env.items()):
        raise ValueError(f"{path}: recorded env must map names to strings")
    return {
        "commands": commands,
        "cwd": cwd,
        "env": env,
        "interpreter": actions.get("interpreter"),
        "source": str(path),
        "sha256": hashlib.sha256(data).hexdigest(),
        "total_actions": len(actions["steps"]),
    }


def race_budget_us(results):
    """The Frozen arm's measured owner p50: median over repeats of the quiet
    QD1 random cohorts' merged clat p50 (bucket lower bound, ns) in
    reads-memory, rounded to microseconds. Fails without such cohorts."""
    case = results.get("cases", {}).get("reads-memory")
    if not results.get("success") or not case:
        raise ValueError("race budget needs a successful frozen reads-memory case")
    p50s = []
    for cohort in case["cohorts"]:
        if cohort["load"] == "quiet" and cohort["name"].endswith("-random-qd1"):
            p50s.append(cohort["fio"]["read"]["clat_ns"]["percentile"]["50.000000"])
    if not p50s:
        raise ValueError("no quiet random QD1 cohort in frozen reads-memory")
    return max(1, round(statistics.median(p50s) / 1000)), p50s


# ---------------------------------------------------------------- resume


def unit_state(unit_dir):
    """What a unit directory says about its unit: absent, done (a success
    receipt), failed (a results file without success) or partial (outputs but
    no results file, e.g. an interrupted run)."""
    unit_dir = Path(unit_dir)
    if not unit_dir.exists():
        return "absent"
    for name in ("controller/results.json", "native/outcome.json", "r0.json"):
        path = unit_dir / name
        if path.exists():
            try:
                return "done" if json.loads(path.read_text()).get("success") else "failed"
            except ValueError:
                return "failed"
    return "partial"


def decide(state, redo):
    """run, skip (already done) or refuse (outputs exist and no --redo)."""
    if state == "absent":
        return "run"
    if state == "done" and not redo:
        return "skip"
    return "run" if redo else "refuse"


# ---------------------------------------------------------------- runner build


def wrapper_paths(text):
    """The build-info JSON and runner.py paths referenced by a cas-two-host
    or cas-native-run wrapper script."""
    build = re.search(r"--build-info (\S+)", text)
    runner = re.search(r"python3 (\S+)/runner\.py", text)
    return {
        "build_info": build[1] if build else None,
        "runner": f"{runner[1]}/runner.py" if runner else None,
    }


def runner_supports(runner_text):
    """Which measurement settings the built two-host runner knows."""
    return {key: bool(re.search(rf'"{key}":', runner_text)) for key in RUNNER_SETTINGS}


def host_output(node, campaign, unit):
    """Where a member's two-host agent writes for a unit: the same relative
    layout under every node's output root, so pulling node N's tree into the
    controller's campaign directory needs no renaming."""
    return f"{node['output_root']}/{campaign}/units/{unit}/host-{node['name']}"


def two_host_config(arm, nodes, campaign, unit, case, fills):
    """A runner configuration for one unit: the arm's settings and one case,
    with fresh storage/output paths under each node's mount and output root
    and the pinning set for that node's type."""
    settings = substitute(arm["config"]["settings"], fills)
    members = []
    for index, node in enumerate(nodes):
        sets = arm["pinning"][node["node_type"]]
        member = {
            "storage": f"{node['storage_mount']}/{campaign}/{unit}",
            "output": host_output(node, campaign, unit),
            "get": f"{node['address']}:{node.get('get_port', 17483)}",
            "bulk": f"{node['address']}:{node.get('bulk_port', 17485)}",
            "cpus": pin_for(sets, "daemon", index),
            "guest_cpus": pin_for(sets, "guests", index),
        }
        if node.get("zfs_parent"):
            member["zfs_parent"] = node["zfs_parent"]
            member["zfs_port"] = node.get("zfs_port", 17487)
        if node.get("ssh"):
            member["ssh"] = node["ssh"]
            member["runner"] = node["runner"]
        members.append(member)
    return {"settings": settings, "nodes": members, "cases": [case]}


REVISION_PLACEHOLDER = "INTEGRATION_REVISION_PLACEHOLDER"
SMT_OFF = ("off", "forceoff", "notsupported", "notimplemented")


def pinned_revision(nodes_config):
    """The revision a native campaign is bound to. Loopback development
    layouts may leave it out; a real campaign must replace the placeholder
    with the integration branch's measured commit before `run`."""
    value = nodes_config.get("expected_revision")
    if loopback(nodes_config) and value in (None, REVISION_PLACEHOLDER):
        return None
    if value is None or value == REVISION_PLACEHOLDER:
        raise ValueError("expected_revision is the placeholder; pin the integration branch's measured revision")
    if not re.fullmatch(r"[0-9a-f]{40}", value):
        raise ValueError("expected_revision must be a full 40-hex commit")
    return value


def smt_problem(inventory, loopback_layout):
    """Pinning assumes physical cores only: a native node must report SMT off."""
    state = (inventory.get("hardware") or {}).get("smt")
    if loopback_layout or state in SMT_OFF:
        return None
    return f"SMT is {state!r}; setup.sh disables it (nosmt) and the pinning sets assume physical cores"


def validate_nodes(nodes_config):
    """The campaign's node description: two or more members, the first local."""
    if not isinstance(nodes_config, dict):
        raise ValueError("nodes config must be an object")
    for key in ("campaign", "nodes"):
        if key not in nodes_config:
            raise ValueError(f"nodes config needs {key!r}")
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]*", nodes_config["campaign"]):
        raise ValueError("campaign name must be a lowercase slug")
    nodes = nodes_config["nodes"]
    if not isinstance(nodes, list) or not 2 <= len(nodes) <= 16:
        raise ValueError("nodes needs 2 to 16 members")
    for index, node in enumerate(nodes):
        for key in ("name", "node_type", "address", "source", "runner", "storage_mount", "output_root"):
            if key not in node:
                raise ValueError(f"node {index} needs {key!r}")
        if not re.fullmatch(r"[a-z0-9]+", node["name"]):
            raise ValueError(f"node {index} name must be a short lowercase slug")
        if index == 0 and node.get("ssh"):
            raise ValueError("node 0 is the local controller and takes no ssh target")
        if index > 0 and not node.get("ssh") and not loopback(nodes_config):
            raise ValueError(f"node {index} needs an ssh target (or a loopback layout)")
        for key in ("storage_mount", "output_root", "source", "runner"):
            if not str(node[key]).startswith("/"):
                raise ValueError(f"node {index} {key} must be absolute")
    return nodes


def loopback(nodes_config):
    """A development layout: every member on the controller's host, no ssh.
    Evidence from such a layout is labelled development, never native."""
    return bool(nodes_config.get("loopback"))


def status_lines(status, width=100):
    """One-screen status: header, counts, current unit, then one line per unit."""
    units = status.get("units", {})
    counts = {}
    for entry in units.values():
        counts[entry["state"]] = counts.get(entry["state"], 0) + 1
    lines = [
        f"campaign {status.get('campaign')} on {status.get('host')} pid {status.get('pid')} launched {status.get('launched')}",
        f"revision {status.get('revision')} phase {status.get('phase')} current {status.get('current') or '-'}",
        "units: " + ", ".join(f"{k} {v}" for k, v in sorted(counts.items())) + f" of {len(units)}",
        f"export {status.get('export') or '-'} last {status.get('last_export') or '-'}",
    ]
    for unit, entry in units.items():
        note = entry.get("error") or entry.get("note") or ""
        lines.append(f"  {entry['state']:8} {unit:44} {entry.get('seconds', ''):>7} {note}"[:width])
    return lines


# ---------------------------------------------------------------- day-four repeats


def unit_p99(unit_dir, runner, case):
    """One p99-class number per finished unit, in nanoseconds, or None with a
    reason. reads cases: the quiet random QD1 cohort's clat p99; burst: the
    slowest guest's cold time to working set; resume: time to working set
    (downtime if no working set ran); r0: node 0's 4 KiB read clat p99;
    native: the read-q1 aggregate's largest per-guest p99."""
    unit_dir = Path(unit_dir)
    try:
        if runner == "two-host":
            results = json.loads((unit_dir / "controller" / "results.json").read_text())
            data = results["cases"][case]
            if case.startswith("reads-"):
                values = [c["fio"]["read"]["clat_ns"]["percentile"]["99.000000"] for c in data["cohorts"] if c["load"] == "quiet" and c["name"].endswith("-random-qd1")]
                return (max(values), None) if values else (None, "no quiet random QD1 cohort")
            if case == "burst-images":
                ttw = data["intervals"]["time_to_working_set_ns"]
                values = [v for v in (ttw.values() if isinstance(ttw, dict) else [ttw]) if isinstance(v, (int, float))]
                return (max(values), None) if values else (None, "no time_to_working_set_ns")
            if case == "resume-elsewhere":
                value = data.get("intervals", {}).get("time_to_working_set_ns")
                if isinstance(value, (int, float)):
                    return value, None
                return data["migration"]["downtime_ns"], "downtime_ns (no working set)"
            return None, f"no p99 rule for case {case}"
        if runner == "r0":
            r0 = json.loads((unit_dir / "r0.json").read_text())
            node = next(iter(r0["nodes"].values()))
            return node["read_4k_qd1"]["read"]["clat_ns"]["percentile"]["99.000000"], None
        if runner == "native":
            aggregate = json.loads((unit_dir / "native" / "aggregate-read-q1.json").read_text())
            return aggregate["read"]["clat_ns_percentile"]["99"]["max"], None
    except (OSError, ValueError, KeyError, TypeError, StopIteration) as error:
        return None, f"{type(error).__name__}: {error}"
    return None, f"no p99 rule for runner {runner}"


def spread(values):
    """(max - min) / median; 0 for fewer than two values."""
    if len(values) < 2:
        return 0.0
    return (max(values) - min(values)) / statistics.median(values)


def plan_repeats(records, threshold=0.15):
    """Day-four units: every (arm, case) whose p99 spread across its finished
    repetitions exceeds `threshold` is scheduled again with the same number
    of repetitions. `records` rows: {day, arm, case, rep, p99, reason}.
    Returns (plan, report)."""
    groups = {}
    for row in records:
        groups.setdefault((row["day"], row["arm"], row["case"]), []).append(row)
    units, report = [], []
    for (day, arm, case), rows in sorted(groups.items()):
        values = [r["p99"] for r in rows if r.get("p99") is not None]
        entry = {"day": day, "arm": arm, "case": case, "finished": len(rows), "measured": len(values), "p99_ns": values}
        if not values:
            entry.update(spread=None, repeat=False, reason="; ".join(sorted({r.get("reason") or "no p99" for r in rows})))
        else:
            value = spread(values)
            entry.update(spread=round(value, 4), repeat=value > threshold)
            if value > threshold:
                units.append([arm, case, max(r["rep"] for r in rows)])
        report.append(entry)
    plan = {"description": f"Repeats of units whose p99 spread across repetitions exceeded {threshold:.0%}; generated by campaign.py repeats.", "days": [{"day": 4, "units": units}]} if units else {"description": f"No unit exceeded {threshold:.0%} p99 spread.", "days": []}
    return plan, report
