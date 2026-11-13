#!/usr/bin/env bash
# R0: the testbed's own floor. fio 4 KiB QD1 direct random reads and 4 KiB
# QD1 write+fdatasync on the experiment XFS, a TCP ping-pong between the two
# nodes (sockperf if installed, else the labelled Python loop in pingpong.py),
# and optionally the kernel nvme-tcp floor: B exports a file-backed nvmet
# namespace on its experiment XFS over the experiment link, A connects with
# nvme-tcp, runs the same 4 KiB QD1 direct read against the block device,
# disconnects, and B tears the target down. Writes r0.json with percentiles.
# Never formats anything; scratch files live in a fresh directory under
# --mount and are removed. The nvme-tcp phase needs root on both nodes and
# refuses to run when nvmet already has subsystems or ports configured.
set -euo pipefail

usage() {
  cat <<'EOF'
usage: r0.sh --mount DIR --output DIR [--source DIR] [--cpus SET] [--seconds N]
             [--role server|client|none --peer HOST:PORT] [--label native|development]
             [--fio PATH] [--size 1G]
             [--nvme-tcp] [--nvme-port 4420] [--nvme-size 1G]
       r0.sh --check [--source DIR]      # report which fio and ping-pong tools would be used
       r0.sh --nvme-tcp-check            # self-check of the nvme-tcp phase's prerequisites, no changes
EOF
  exit 2
}

MOUNT= OUTPUT= SOURCE= CPUS= SECONDS_=30 ROLE=none PEER= LABEL=native FIO= SIZE=1G CHECK=0
NVME_TCP=0 NVME_PORT=4420 NVME_SIZE=1G NVME_CHECK=0 NQN=nqn.2026-11.sh.harivan.cas:r0-floor
while [[ $# -gt 0 ]]; do
  case "$1" in
    --mount) MOUNT=$2; shift ;;
    --output) OUTPUT=$2; shift ;;
    --source) SOURCE=$2; shift ;;
    --cpus) CPUS=$2; shift ;;
    --seconds) SECONDS_=$2; shift ;;
    --role) ROLE=$2; shift ;;
    --peer) PEER=$2; shift ;;
    --label) LABEL=$2; shift ;;
    --fio) FIO=$2; shift ;;
    --size) SIZE=$2; shift ;;
    --check) CHECK=1 ;;
    --nvme-tcp) NVME_TCP=1 ;;
    --nvme-port) NVME_PORT=$2; shift ;;
    --nvme-size) NVME_SIZE=$2; shift ;;
    --nvme-tcp-check) NVME_CHECK=1 ;;
    *) usage ;;
  esac
  shift
done
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

nix_env() {
  # shellcheck disable=SC1091
  { [[ -r /etc/profile.d/nix.sh ]] && . /etc/profile.d/nix.sh; } || true
  # shellcheck disable=SC1091
  { [[ -r ${HOME:-/root}/.nix-profile/etc/profile.d/nix.sh ]] && . "${HOME:-/root}/.nix-profile/etc/profile.d/nix.sh"; } || true
  # shellcheck disable=SC1091
  { [[ -r /nix/var/nix/profiles/default/etc/profile.d/nix.sh ]] && . /nix/var/nix/profiles/default/etc/profile.d/nix.sh; } || true
  # shellcheck disable=SC1091
  { [[ -r /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh ]] && . /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh; } || true
}
find_fio() {
  if [[ -n $FIO ]]; then echo "$FIO"; return; fi
  if command -v fio >/dev/null; then command -v fio; return; fi
  nix_env
  if [[ -n $SOURCE && -d $SOURCE ]] && command -v nix >/dev/null; then
    # The flake's dev shell pins fio; resolve it once so the measurement runs the binary directly.
    local path
    path=$(cd "$SOURCE" && nix develop --quiet -c sh -c 'command -v fio' 2>/dev/null || true)
    [[ -n $path ]] && { echo "$path"; return; }
  fi
  echo ""
}
pingpong_tool() {
  if command -v sockperf >/dev/null; then echo "sockperf"; else echo "python-tcp-pingpong"; fi
}

NVMET=/sys/kernel/config/nvmet
# ---------------------------------------------------------------- nvme-tcp floor
nvme_prereqs() {  # prints one line per check: name=ok|missing[: detail]; exit 1 if any missing
  local ok=0
  for m in nvmet nvmet-tcp nvme-tcp nvme-fabrics; do
    if modinfo "$m" >/dev/null 2>&1; then echo "module-$m=ok"; else echo "module-$m=missing"; ok=1; fi
  done
  if command -v nvme >/dev/null; then echo "nvme-cli=ok $(nvme version 2>/dev/null | head -1)"; else echo "nvme-cli=missing"; ok=1; fi
  if [[ $(id -u) == 0 ]]; then echo "root=ok"; else echo "root=missing"; ok=1; fi
  if mountpoint -q /sys/kernel/config || mount -t configfs configfs /sys/kernel/config 2>/dev/null; then echo "configfs=ok"; else echo "configfs=missing"; ok=1; fi
  if [[ -d $NVMET ]]; then
    local subs ports
    subs=$(ls "$NVMET/subsystems" 2>/dev/null | wc -l); ports=$(ls "$NVMET/ports" 2>/dev/null | wc -l)
    if (( subs == 0 && ports == 0 )); then echo "nvmet-unconfigured=ok"; else echo "nvmet-unconfigured=missing: $subs subsystems, $ports ports already configured"; ok=1; fi
  else
    echo "nvmet-configfs=not-loaded (loaded by modprobe at run time)"
  fi
  return $ok
}
nvmet_attrs() {  # record every attribute under a configfs directory
  local dir=$1
  find "$dir" -maxdepth 2 -type f 2>/dev/null | while read -r f; do printf '%s=%s\n' "${f#"$NVMET"/}" "$(tr -d '\0' < "$f" 2>/dev/null | head -c 200 | tr '\n' ' ')"; done
}
nvmet_up() {  # B: file-backed namespace on the experiment XFS, exported on the experiment address
  modprobe nvmet && modprobe nvmet-tcp
  nvme_prereqs > "$OUTPUT/nvme-tcp-prereqs.txt" || { echo "nvme-tcp prerequisites missing (see nvme-tcp-prereqs.txt)"; return 1; }
  local file=$scratch/nvmet.img sub=$NVMET/subsystems/$NQN port=$NVMET/ports/1
  fallocate -l "$NVME_SIZE" "$file"
  "$FIO_BIN" --name=nvmet-fill --rw=write --bs=1M --direct=1 --ioengine=psync --filename="$file" --size="$NVME_SIZE" --output-format=json > "$OUTPUT/fio-nvmet-fill.json"
  mkdir -p "$sub/namespaces/1"
  echo 1 > "$sub/attr_allow_any_host"
  echo -n "$file" > "$sub/namespaces/1/device_path"
  echo 0 > "$sub/namespaces/1/buffered_io"
  echo 1 > "$sub/namespaces/1/enable"
  mkdir -p "$port"
  echo tcp > "$port/addr_trtype"; echo ipv4 > "$port/addr_adrfam"
  echo -n "${PEER%:*}" > "$port/addr_traddr"; echo -n "$NVME_PORT" > "$port/addr_trsvcid"
  ln -s "$sub" "$port/subsystems/$NQN"
  { nvmet_attrs "$sub"; nvmet_attrs "$sub/namespaces/1"; nvmet_attrs "$port"; echo "file=$file size=$NVME_SIZE"; } > "$OUTPUT/nvmet-target.txt"
  dmesg | tail -20 > "$OUTPUT/nvmet-dmesg.txt" 2>/dev/null || true
}
nvmet_down() {
  local sub=$NVMET/subsystems/$NQN port=$NVMET/ports/1
  rm -f "$port/subsystems/$NQN"; rmdir "$port" 2>/dev/null || true
  [[ -e $sub/namespaces/1/enable ]] && echo 0 > "$sub/namespaces/1/enable"
  rmdir "$sub/namespaces/1" "$sub" 2>/dev/null || true
  ls "$NVMET/subsystems" "$NVMET/ports" > "$OUTPUT/nvmet-after-teardown.txt" 2>&1 || true
}
nvme_client() {  # A: connect, record the controller, read, disconnect
  modprobe nvme-tcp
  nvme_prereqs > "$OUTPUT/nvme-tcp-prereqs.txt" || { echo "nvme-tcp prerequisites missing (see nvme-tcp-prereqs.txt)"; return 1; }
  python3 "$HERE/pingpong.py" wait "${PEER%:*}:$((${PEER##*:} + 1))"
  nvme connect -t tcp -a "${PEER%:*}" -s "$NVME_PORT" -n "$NQN" > "$OUTPUT/nvme-connect.txt" 2>&1
  local ctrl= dev= i
  for i in $(seq 1 60); do
    for c in /sys/class/nvme/nvme*; do
      [[ $(cat "$c/subsysnqn" 2>/dev/null) == "$NQN" ]] && ctrl=$c && break
    done
    [[ -n $ctrl ]] && dev=$(ls -d "$ctrl"/nvme*n1 2>/dev/null | head -1) && [[ -n $dev && -b /dev/$(basename "$dev") ]] && break
    sleep 1
  done
  [[ -n $dev ]] || { echo "nvme-tcp namespace did not appear"; nvme disconnect -n "$NQN" >/dev/null 2>&1 || true; return 1; }
  dev=/dev/$(basename "$dev")
  { for a in transport address state sqsize queue_count numa_node model serial firmware_rev cntlid; do printf '%s=%s\n' "$a" "$(cat "$ctrl/$a" 2>/dev/null)"; done
    echo "device=$dev"; nvme id-ctrl "$dev" 2>/dev/null | head -40; cat /sys/module/nvme_tcp/parameters/* 2>/dev/null | head -5; } > "$OUTPUT/nvme-tcp-initiator.txt"
  "${pin[@]}" "$FIO_BIN" --name=nvme-tcp-read-4k-qd1 --rw=randread --output-format=json+ --percentile_list=50:90:99:99.9:99.99 \
    --direct=1 --bs=4k --iodepth=1 --ioengine=psync --filename="$dev" --readonly --time_based --runtime="$SECONDS_" --randrepeat=0 --norandommap \
    > "$OUTPUT/fio-nvme-tcp-read-4k-qd1.json" || rc=1
  nvme disconnect -n "$NQN" > "$OUTPUT/nvme-disconnect.txt" 2>&1 || rc=1
  python3 "$HERE/pingpong.py" done "${PEER%:*}:$((${PEER##*:} + 1))"
}

FIO_BIN=$(find_fio)
TOOL=$(pingpong_tool)
if [[ $NVME_CHECK == 1 ]]; then
  nvme_prereqs; exit
fi
if [[ $CHECK == 1 ]]; then
  echo "fio: ${FIO_BIN:-missing}"
  echo "pingpong: $TOOL"
  [[ -n $FIO_BIN ]]
  exit
fi
[[ -n $MOUNT && -n $OUTPUT ]] || usage
[[ -n $FIO_BIN ]] || { echo "no fio found; pass --fio or --source"; exit 1; }
[[ $ROLE == none || -n $PEER ]] || usage

mkdir -p "$OUTPUT"
pin=()
[[ -n $CPUS ]] && pin=(taskset -c "$CPUS")
fstype=$(findmnt -no FSTYPE -T "$MOUNT")
[[ $fstype == xfs ]] || { echo "$MOUNT is $fstype, not xfs"; exit 1; }
scratch=$MOUNT/r0-$(date -u +%Y%m%dT%H%M%SZ)-$$
mkdir "$scratch"
trap 'rm -rf "$scratch"' EXIT
{
  echo "hostname=$(hostname)"; echo "label=$LABEL"; echo "cpus=$CPUS"; echo "fio=$FIO_BIN"; echo "fio_version=$("$FIO_BIN" --version)"
  echo "mount=$(findmnt -J -T "$MOUNT" -o TARGET,SOURCE,FSTYPE,OPTIONS)"; echo "scratch=$scratch"; echo "role=$ROLE peer=$PEER tool=$TOOL"
  echo "kernel=$(uname -r)"; echo "loadavg=$(cat /proc/loadavg)"
} > "$OUTPUT/env.txt"

# The server listens before its own fio phase so the client never waits on it.
server_pid=
if [[ $ROLE == server ]]; then
  if [[ $TOOL == sockperf ]]; then
    timeout 900 sockperf server -i "${PEER%:*}" -p "${PEER##*:}" > "$OUTPUT/pingpong-server.txt" 2>&1 &
  else
    timeout 900 python3 "$HERE/pingpong.py" server "$PEER" > "$OUTPUT/pingpong-server.txt" 2>&1 &
  fi
  server_pid=$!
fi

fio_common=(--output-format=json+ --percentile_list=50:90:99:99.9:99.99 --direct=1 --bs=4k --iodepth=1 --ioengine=psync --filename="$scratch/file" --size="$SIZE" --time_based --runtime="$SECONDS_" --group_reporting --randrepeat=0 --norandommap --refill_buffers)
rc=0
echo "fill $SIZE" >> "$OUTPUT/env.txt"
"${pin[@]}" "$FIO_BIN" --name=fill --rw=write --bs=1M --direct=1 --ioengine=psync --filename="$scratch/file" --size="$SIZE" --output-format=json > "$OUTPUT/fio-fill.json" || rc=1
"${pin[@]}" "$FIO_BIN" --name=read-4k-qd1 --rw=randread "${fio_common[@]}" > "$OUTPUT/fio-read-4k-qd1.json" || rc=1
"${pin[@]}" "$FIO_BIN" --name=fdatasync-4k-qd1 --rw=randwrite --fdatasync=1 "${fio_common[@]}" > "$OUTPUT/fio-fdatasync-4k-qd1.json" || rc=1

nvme_status=skipped
case "$ROLE" in
  server)
    if [[ $TOOL == sockperf ]]; then wait "$server_pid" || true; else wait "$server_pid" || rc=1; fi
    if [[ $NVME_TCP == 1 ]]; then
      if nvmet_up; then
        # The gate tells A the target is ready and waits for A's "done".
        timeout 900 python3 "$HERE/pingpong.py" gate "${PEER%:*}:$((${PEER##*:} + 1))" && nvme_status=ok || { nvme_status=failed; rc=1; }
      else nvme_status=failed; rc=1; fi
      nvmet_down
    fi ;;
  client)
    if [[ $TOOL == sockperf ]]; then
      "${pin[@]}" sockperf ping-pong -i "${PEER%:*}" -p "${PEER##*:}" -t "$SECONDS_" --full-rtt --msg-size 64 > "$OUTPUT/pingpong-sockperf.txt" 2>&1 || rc=1
    else
      "${pin[@]}" python3 "$HERE/pingpong.py" client "$PEER" --seconds "$SECONDS_" > "$OUTPUT/pingpong.json" 2> "$OUTPUT/pingpong.err" || rc=1
    fi
    if [[ $NVME_TCP == 1 ]]; then
      if nvme_client > "$OUTPUT/nvme-tcp.log" 2>&1; then nvme_status=ok; else nvme_status=failed; rc=1; fi
    fi ;;
esac
echo "nvme_tcp=$nvme_status" >> "$OUTPUT/env.txt"

python3 - "$OUTPUT" "$rc" "$LABEL" "$ROLE" "$TOOL" "$nvme_status" <<'PY'
import json, pathlib, re, sys
out, rc, label, role, tool, nvme_status = pathlib.Path(sys.argv[1]), int(sys.argv[2]), sys.argv[3], sys.argv[4], sys.argv[5], sys.argv[6]
def fio(name):
    path = out / f"fio-{name}.json"
    try:
        job = json.loads(path.read_text())["jobs"][0]
    except (OSError, ValueError, KeyError, IndexError) as error:
        return {"error": f"{path.name}: {error}"}
    rows = {}
    for direction in ("read", "write"):
        if job[direction]["total_ios"]:
            rows[direction] = {"iops": job[direction]["iops"], "total_ios": job[direction]["total_ios"], "clat_ns": {k: job[direction]["clat_ns"][k] for k in ("min", "max", "mean", "percentile")}}
    sync = job.get("sync", {})
    # fio 3.41 leaves sync.total_ios at 0 and counts the samples in lat_ns.N.
    if sync.get("lat_ns", {}).get("N") or sync.get("total_ios"):
        rows["fdatasync"] = {"samples": sync["lat_ns"].get("N", sync.get("total_ios")), "lat_ns": {k: sync["lat_ns"][k] for k in ("min", "max", "mean", "percentile")}}
    rows["errors"] = job.get("error", 0)
    return rows
pingpong = None
if role == "client":
    if tool == "sockperf":
        text = (out / "pingpong-sockperf.txt").read_text() if (out / "pingpong-sockperf.txt").exists() else ""
        pingpong = {"tool": "sockperf", "raw": "pingpong-sockperf.txt", "percentile_usec": {m[1]: float(m[2]) for m in re.finditer(r"percentile\s+([\d.]+)\s*=\s*([\d.]+)", text)}, "avg_rtt_usec": (lambda m: float(m[1]) if m else None)(re.search(r"avg-rtt=([\d.]+)", text))}
    else:
        try:
            pingpong = json.loads((out / "pingpong.json").read_text())
        except (OSError, ValueError) as error:
            pingpong = {"error": str(error)}
def text(name):
    return (out / name).read_text() if (out / name).exists() else None
nvme = None
if nvme_status != "skipped":
    nvme = {"status": nvme_status, "role": role, "prerequisites": text("nvme-tcp-prereqs.txt"),
            "target": text("nvmet-target.txt"), "teardown": text("nvmet-after-teardown.txt"),
            "initiator": text("nvme-tcp-initiator.txt"), "connect": text("nvme-connect.txt"), "disconnect": text("nvme-disconnect.txt"),
            "read_4k_qd1": fio("nvme-tcp-read-4k-qd1") if role == "client" else None}
result = {"success": rc == 0, "evidence_label": label, "role": role, "env": (out / "env.txt").read_text(), "read_4k_qd1": fio("read-4k-qd1"), "fdatasync_4k_qd1": fio("fdatasync-4k-qd1"), "pingpong": pingpong, "nvme_tcp": nvme}
(out / "r0.json").write_text(json.dumps(result, indent=1) + "\n")
PY
exit $rc
