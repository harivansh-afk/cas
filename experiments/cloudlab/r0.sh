#!/usr/bin/env bash
# R0: the testbed's own floor. fio 4 KiB QD1 direct random reads and 4 KiB
# QD1 write+fdatasync on the experiment XFS, and a TCP ping-pong between the
# two nodes (sockperf if installed, else the labelled Python loop in
# pingpong.py). Writes r0.json with percentiles. Never formats anything; its
# scratch file lives in a fresh directory under --mount and is removed.
set -euo pipefail

usage() {
  cat <<'EOF'
usage: r0.sh --mount DIR --output DIR [--source DIR] [--cpus SET] [--seconds N]
             [--role server|client|none --peer HOST:PORT] [--label native|development]
             [--fio PATH] [--size 1G]
       r0.sh --check [--source DIR]      # report which fio and ping-pong tools would be used
EOF
  exit 2
}

MOUNT= OUTPUT= SOURCE= CPUS= SECONDS_=30 ROLE=none PEER= LABEL=native FIO= SIZE=1G CHECK=0
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
    *) usage ;;
  esac
  shift
done
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

nix_env() {
  # shellcheck disable=SC1091
  { [[ -r /etc/profile.d/nix.sh ]] && . /etc/profile.d/nix.sh; } || true
  # shellcheck disable=SC1091
  { [[ -r ~/.nix-profile/etc/profile.d/nix.sh ]] && . ~/.nix-profile/etc/profile.d/nix.sh; } || true
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

FIO_BIN=$(find_fio)
TOOL=$(pingpong_tool)
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

case "$ROLE" in
  server)
    if [[ $TOOL == sockperf ]]; then wait "$server_pid" || true; else wait "$server_pid" || rc=1; fi ;;
  client)
    if [[ $TOOL == sockperf ]]; then
      "${pin[@]}" sockperf ping-pong -i "${PEER%:*}" -p "${PEER##*:}" -t "$SECONDS_" --full-rtt --msg-size 64 > "$OUTPUT/pingpong-sockperf.txt" 2>&1 || rc=1
    else
      "${pin[@]}" python3 "$HERE/pingpong.py" client "$PEER" --seconds "$SECONDS_" > "$OUTPUT/pingpong.json" 2> "$OUTPUT/pingpong.err" || rc=1
    fi ;;
esac

python3 - "$OUTPUT" "$rc" "$LABEL" "$ROLE" "$TOOL" <<'PY'
import json, pathlib, re, sys
out, rc, label, role, tool = pathlib.Path(sys.argv[1]), int(sys.argv[2]), sys.argv[3], sys.argv[4], sys.argv[5]
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
    if job.get("sync", {}).get("total_ios"):
        rows["fdatasync"] = {"total_ios": job["sync"]["total_ios"], "lat_ns": {k: job["sync"]["lat_ns"][k] for k in ("min", "max", "mean", "percentile")}}
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
result = {"success": rc == 0, "evidence_label": label, "role": role, "env": (out / "env.txt").read_text(), "read_4k_qd1": fio("read-4k-qd1"), "fdatasync_4k_qd1": fio("fdatasync-4k-qd1"), "pingpong": pingpong}
(out / "r0.json").write_text(json.dumps(result, indent=1) + "\n")
PY
exit $rc
