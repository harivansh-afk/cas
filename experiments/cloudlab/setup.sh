#!/usr/bin/env bash
# Per-node CloudLab setup: record hardware before touching anything, install
# Nix, build the pinned revision from a git bundle, make one XFS on the named
# experiment disk, raise the descriptor limit, pin the experiment NIC's IRQs,
# and write node-inventory.json. Idempotent; every step is logged with the
# hashes of what it produced. See docs/cloudlab-campaign.md for what it
# refuses to do.
set -euo pipefail

usage() {
  cat <<'EOF'
usage: setup.sh [--inventory-only] [--out FILE]
                [--device /dev/disk/by-id/X --mount DIR] [--skip-disk]
                [--bundle FILE --revision SHA --source DIR] [--skip-build] [--skip-nix]
                [--nic IFACE --irq-cpus SET] [--skip-irq] [--skip-smt]
                [--user NAME] [--records DIR] [--nix-installer FILE] [--cores N]

  --inventory-only  record hardware/settings and write the inventory; change nothing
  --device          whole-disk block device to format as the experiment XFS (never a
                    partition, never a disk with any filesystem signature or mount)
  --mount           mount point for that filesystem (default /mnt/cas-exp)
  --bundle          git bundle holding --revision; cloned/fetched into --source
  --nic             experiment NIC whose IRQs are pinned to --irq-cpus
  --user            experiment account that owns the mount (default: invoking user)
  --records         directory for raw records (default <out dir>/setup-records)
EOF
  exit 2
}

DEVICE= MOUNT=/mnt/cas-exp BUNDLE= REVISION= SOURCE= NIC= IRQ_CPUS= OUT=node-inventory.json
USER_NAME=${SUDO_USER:-$(id -un)} RECORDS= INSTALLER= CORES=4
INVENTORY_ONLY=0 SKIP_DISK=0 SKIP_BUILD=0 SKIP_NIX=0 SKIP_IRQ=0 SKIP_SMT=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --inventory-only) INVENTORY_ONLY=1 ;;
    --device) DEVICE=$2; shift ;;
    --mount) MOUNT=$2; shift ;;
    --skip-disk) SKIP_DISK=1 ;;
    --bundle) BUNDLE=$2; shift ;;
    --revision) REVISION=$2; shift ;;
    --source) SOURCE=$2; shift ;;
    --skip-build) SKIP_BUILD=1 ;;
    --skip-nix) SKIP_NIX=1 ;;
    --nic) NIC=$2; shift ;;
    --irq-cpus) IRQ_CPUS=$2; shift ;;
    --skip-irq) SKIP_IRQ=1 ;;
    --skip-smt) SKIP_SMT=1 ;;
    --user) USER_NAME=$2; shift ;;
    --records) RECORDS=$2; shift ;;
    --out) OUT=$2; shift ;;
    --nix-installer) INSTALLER=$2; shift ;;
    --cores) CORES=$2; shift ;;
    *) usage ;;
  esac
  shift
done

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
OUT=$(readlink -f "$(dirname "$OUT")")/$(basename "$OUT")
RECORDS=${RECORDS:-${OUT%.json}-records}
mkdir -p "$RECORDS/hw" "$RECORDS/steps"
LOG=$RECORDS/setup.log
MARKER=.cas-experiment-disk

log() { printf '%s %s\n' "$(date -u +%FT%TZ)" "$*" | tee -a "$LOG" >&2; }
as_root() { if [[ $(id -u) == 0 ]]; then "$@"; else sudo -n "$@"; fi; }
hash_of() { sha256sum "$1" 2>/dev/null | cut -d' ' -f1; }
record() {  # record NAME COMMAND... : stdout to hw/NAME.txt; missing tools recorded, never fatal
  local name=$1; shift
  if "$@" > "$RECORDS/hw/$name.txt" 2> "$RECORDS/hw/$name.err"; then
    rm -f "$RECORDS/hw/$name.err"
  else
    printf 'unavailable: %s (exit %s)\n' "$*" "$?" >> "$RECORDS/hw/$name.err"
  fi
}
step() {  # step NAME FUNCTION : logs start/end/exit and the hashes of the step's record files
  local name=$1 fn=$2 rc=0 started
  started=$(date -u +%FT%TZ)
  log "== $name start"
  "$fn" > "$RECORDS/steps/$name.out" 2>&1 || rc=$?
  log "== $name exit $rc"
  python3 - "$RECORDS" "$name" "$rc" "$started" <<'PY'
import hashlib, json, pathlib, sys, time
records, name, rc, started = pathlib.Path(sys.argv[1]), sys.argv[2], int(sys.argv[3]), sys.argv[4]
files = {}
for path in sorted(records.rglob("*")):
    if path.is_file() and path.name != "setup.log" and not path.name.endswith(".jsonl"):
        files[str(path.relative_to(records))] = hashlib.sha256(path.read_bytes()).hexdigest()
row = {"step": name, "rc": rc, "started": started, "finished": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "files": files}
with (records / "steps.jsonl").open("a") as out:
    out.write(json.dumps(row) + "\n")
PY
  return $rc
}

# ---------------------------------------------------------------- 1. hardware and settings, before anything
hardware() {
  record uname uname -a
  record cmdline cat /proc/cmdline
  record nproc nproc
  record lscpu lscpu -J
  record lscpu-text lscpu
  record meminfo cat /proc/meminfo
  record numa bash -c 'for n in /sys/devices/system/node/node*; do echo "$n $(cat $n/cpulist)"; done'
  record lsblk lsblk -J -b -O
  record mounts findmnt -J -o TARGET,SOURCE,FSTYPE,OPTIONS
  record fstab cat /etc/fstab
  record dmi bash -c 'for f in sys_vendor product_name bios_vendor bios_version bios_date board_name; do printf "%s=%s\n" $f "$(cat /sys/class/dmi/id/$f 2>/dev/null)"; done'
  record dmidecode as_root dmidecode -t bios -t system -t baseboard
  record nvme-list bash -c 'ls /dev/nvme*n1 2>/dev/null'
  for dev in /dev/nvme*n1; do
    [[ -e $dev ]] || continue
    record "nvme-id-ctrl-$(basename "$dev")" as_root nvme id-ctrl -H "$dev"
    record "nvme-id-ns-$(basename "$dev")" as_root nvme id-ns "$dev"
    record "nvme-smart-$(basename "$dev")" as_root nvme smart-log "$dev"
  done
  record ip-link ip -j -d link
  record ip-addr ip -j address
  record interrupts cat /proc/interrupts
  record irqbalance systemctl is-active irqbalance
  record irq-affinity bash -c 'for i in /proc/irq/[0-9]*; do printf "%s %s %s\n" "$(basename $i)" "$(cat $i/smp_affinity_list 2>/dev/null)" "$(ls $i 2>/dev/null | grep -v -e smp_affinity -e node -e spurious -e affinity_hint -e effective | head -1)"; done'
  record cpufreq bash -c 'for c in /sys/devices/system/cpu/cpu[0-9]*; do printf "%s %s %s %s\n" "$(basename $c)" "$(cat $c/cpufreq/scaling_governor 2>/dev/null)" "$(cat $c/cpufreq/scaling_min_freq 2>/dev/null)" "$(cat $c/cpufreq/scaling_max_freq 2>/dev/null)"; done'
  record cpuidle bash -c 'for s in /sys/devices/system/cpu/cpu0/cpuidle/state*; do printf "%s %s latency=%s disable=%s\n" "$(basename $s)" "$(cat $s/name)" "$(cat $s/latency)" "$(cat $s/disable)"; done; echo "driver=$(cat /sys/devices/system/cpu/cpuidle/current_driver 2>/dev/null)"; echo "intel_idle.max_cstate=$(cat /sys/module/intel_idle/parameters/max_cstate 2>/dev/null)"; echo "processor.max_cstate=$(cat /sys/module/processor/parameters/max_cstate 2>/dev/null)"'
  record smt cat /sys/devices/system/cpu/smt/control
  record smt-active cat /sys/devices/system/cpu/smt/active
  record grub-cas bash -c 'cat /etc/default/grub.d/99-cas-nosmt.cfg 2>/dev/null; grep -h CMDLINE /etc/default/grub 2>/dev/null'
  record thp cat /sys/kernel/mm/transparent_hugepage/enabled
  record ulimit bash -c 'ulimit -n; ulimit -Hn'
  record limits-d bash -c 'cat /etc/security/limits.d/*.conf 2>/dev/null'
  record kvm bash -c 'ls -l /dev/kvm; grep -c -E "vmx|svm" /proc/cpuinfo'
  record nix-version bash -c '. /etc/profile.d/nix.sh 2>/dev/null; . ~/.nix-profile/etc/profile.d/nix.sh 2>/dev/null; nix --version'
  record fio-version fio --version
  record zfs-version bash -c 'zfs version; zpool list'
  record uptime-load bash -c 'uptime; cat /proc/loadavg'
  record ps-top bash -c 'ps -eo pid,user,pcpu,pmem,comm --sort=-pcpu | head -25'
  local nics
  nics=$(ls /sys/class/net | grep -v -e '^lo$')
  [[ -n $NIC ]] && nics="$NIC $nics"
  for nic in $nics; do
    record "ethtool-$nic" ethtool "$nic"
    record "ethtool-i-$nic" ethtool -i "$nic"
    record "ethtool-g-$nic" ethtool -g "$nic"
    record "ethtool-c-$nic" ethtool -c "$nic"
    record "ethtool-k-$nic" ethtool -k "$nic"
    record "ethtool-l-$nic" ethtool -l "$nic"
    record "mtu-$nic" cat "/sys/class/net/$nic/mtu"
    record "numa-$nic" cat "/sys/class/net/$nic/device/numa_node"
    record "msi-irqs-$nic" bash -c "ls /sys/class/net/$nic/device/msi_irqs 2>/dev/null"
  done
  printf '{"device":%s,"mount":%s,"nic":%s,"irq_cpus":%s,"revision":%s,"source":%s,"user":%s,"inventory_only":%s,"skip":{"disk":%s,"build":%s,"nix":%s,"irq":%s}}\n' \
    "\"$DEVICE\"" "\"$MOUNT\"" "\"$NIC\"" "\"$IRQ_CPUS\"" "\"$REVISION\"" "\"$SOURCE\"" "\"$USER_NAME\"" "$INVENTORY_ONLY" "$SKIP_DISK" "$SKIP_BUILD" "$SKIP_NIX" "$SKIP_IRQ" \
    > "$RECORDS/args.json"
}

# ---------------------------------------------------------------- 2. nix
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
install_nix() {
  nix_env
  if command -v nix >/dev/null; then
    echo "nix present: $(nix --version)"
  else
    local installer=${INSTALLER:-$RECORDS/nix-installer.sh}
    if [[ ! -s $installer ]]; then
      curl -fsSL https://nixos.org/nix/install -o "$installer"
    fi
    echo "installer sha256 $(hash_of "$installer")"
    as_root sh "$installer" --daemon --yes --no-channel-add
    nix_env
  fi
  if ! grep -qs 'experimental-features.*flakes' /etc/nix/nix.conf 2>/dev/null; then
    printf 'experimental-features = nix-command flakes\nmax-jobs = 2\ncores = %s\n' "$CORES" | as_root tee -a /etc/nix/nix.conf
    as_root systemctl restart nix-daemon 2>/dev/null || true
  fi
  nix --version | tee "$RECORDS/nix.txt"
}

# ---------------------------------------------------------------- 3. source and build
checkout() {
  [[ -n $BUNDLE && -n $REVISION && -n $SOURCE ]] || { echo "--bundle, --revision and --source are required for the build"; return 2; }
  git bundle verify "$BUNDLE"
  if [[ ! -d $SOURCE/.git ]]; then
    git clone --quiet "$BUNDLE" "$SOURCE"
  else
    git -C "$SOURCE" fetch --quiet "$BUNDLE" '+refs/heads/*:refs/remotes/bundle/*'
  fi
  git -C "$SOURCE" checkout --quiet --detach "$REVISION"
  local head
  head=$(git -C "$SOURCE" rev-parse HEAD)
  [[ $head == "$REVISION" ]] || { echo "checked out $head, wanted $REVISION"; return 1; }
  [[ -z $(git -C "$SOURCE" status --porcelain) ]] || { echo "source tree is dirty"; return 1; }
  printf '{"bundle":"%s","bundle_sha256":"%s","revision":"%s","source":"%s"}\n' "$BUNDLE" "$(hash_of "$BUNDLE")" "$head" "$SOURCE" > "$RECORDS/source.json"
}
build() {
  nix_env
  (cd "$SOURCE" && nix build .#two-host --out-link result-two-host --cores "$CORES" --option http2 false)
  (cd "$SOURCE" && nix build .#native --out-link result-native --cores "$CORES" --option http2 false)
  python3 - "$SOURCE" "$RECORDS" <<'PY'
import hashlib, json, pathlib, re, sys
source, records = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
out = {}
for name in ("two-host", "native"):
    wrapper = source / f"result-{name}" / "bin" / ("cas-two-host" if name == "two-host" else "cas-native-run")
    text = wrapper.read_text()
    info = re.search(r"--build-info (\S+)", text)[1]
    build = json.loads(pathlib.Path(info).read_text())
    hashes = {k: hashlib.sha256(pathlib.Path(v).read_bytes()).hexdigest() for k, v in build.items() if isinstance(v, str) and v.startswith("/nix/store") and pathlib.Path(v).is_file()}
    out[name] = {"wrapper": str(wrapper.resolve()), "build_info": info, "source_revision": build.get("source_revision"), "binaries": hashes}
(records / "build.json").write_text(json.dumps(out, indent=1) + "\n")
PY
}

# ---------------------------------------------------------------- 4. the experiment disk
refuse() { echo "REFUSED: $*"; return 1; }
disk() {
  [[ -n $DEVICE ]] || refuse "--device is required (or --skip-disk)"
  [[ $DEVICE == /dev/* ]] || refuse "device must be an absolute /dev path"
  local real type pk root_disk
  real=$(readlink -f "$DEVICE")
  [[ -b $real ]] || refuse "$DEVICE is not a block device"
  type=$(lsblk -dno TYPE "$real")
  [[ $type == disk ]] || refuse "$DEVICE is a $type, not a whole disk; this script never partitions"
  root_disk=$(lsblk -no PKNAME "$(findmnt -no SOURCE /)" 2>/dev/null || true)
  [[ -n $root_disk && /dev/$root_disk == "$real" ]] && refuse "$DEVICE holds the OS root"
  for mp in /boot /boot/efi /nix /home /var; do
    pk=$(lsblk -no PKNAME "$(findmnt -no SOURCE "$mp" 2>/dev/null)" 2>/dev/null || true)
    [[ -n $pk && /dev/$pk == "$real" ]] && refuse "$DEVICE holds $mp"
  done
  lsblk -J -o NAME,PATH,TYPE,FSTYPE,MOUNTPOINTS,LABEL,UUID "$real" > "$RECORDS/disk-before.json"
  as_root wipefs -n "$real" > "$RECORDS/disk-signatures.txt" 2>&1 || true
  local children fstype mounted
  children=$(lsblk -no NAME "$real" | tail -n +2)
  fstype=$(lsblk -dno FSTYPE "$real")
  mounted=$(lsblk -no MOUNTPOINTS "$real" | grep -v '^$' || true)
  if [[ $fstype == xfs && -z $children ]]; then
    local tmp=$MOUNT
    if [[ -z $mounted ]]; then
      as_root mkdir -p "$MOUNT"; as_root mount -o noatime "$real" "$MOUNT"; mounted=$MOUNT
    else
      tmp=$mounted
    fi
    if [[ -f $tmp/$MARKER ]] && grep -q "^host=$(hostname)" "$tmp/$MARKER"; then
      echo "reusing our experiment filesystem on $real (marker $(cat "$tmp/$MARKER" | tr '\n' ' '))"
      [[ $tmp == "$MOUNT" ]] || refuse "$DEVICE is ours but mounted at $tmp, not $MOUNT"
      write_disk_json "$real" reused; return 0
    fi
    [[ $tmp == "$MOUNT" && -z $(ls -A "$MOUNT" 2>/dev/null) ]] && as_root umount "$MOUNT"
    refuse "$DEVICE holds an XFS filesystem without our marker; it may contain data"
  fi
  [[ -z $children ]] || refuse "$DEVICE has partitions/children ($children); will not touch a disk that is in use"
  [[ -z $fstype ]] || refuse "$DEVICE holds a $fstype filesystem"
  [[ -z $mounted ]] || refuse "$DEVICE is mounted at $mounted"
  [[ ! -s $RECORDS/disk-signatures.txt ]] || refuse "$DEVICE carries signatures: $(cat "$RECORDS/disk-signatures.txt")"
  grep -qs "$real" /proc/mdstat && refuse "$DEVICE is a RAID member"
  as_root mkfs.xfs -q "$real"
  as_root mkdir -p "$MOUNT"
  as_root mount -o noatime "$real" "$MOUNT"
  printf 'host=%s\ndevice=%s\ncreated=%s\nserial=%s\n' "$(hostname)" "$real" "$(date -u +%FT%TZ)" "$(lsblk -dno SERIAL "$real")" | as_root tee "$MOUNT/$MARKER" >/dev/null
  as_root chown "$USER_NAME" "$MOUNT"
  write_disk_json "$real" created
}
write_disk_json() {
  lsblk -J -o NAME,PATH,TYPE,FSTYPE,MOUNTPOINTS,LABEL,UUID,SERIAL,MODEL,SIZE "$1" > "$RECORDS/disk-after.json"
  findmnt -J -T "$MOUNT" -o TARGET,SOURCE,FSTYPE,OPTIONS,UUID > "$RECORDS/disk-mount.json"
  printf '{"device":"%s","real":"%s","mount":"%s","state":"%s","marker":"%s"}\n' "$DEVICE" "$1" "$MOUNT" "$2" "$(cat "$MOUNT/$MARKER" | tr '\n' ';')" > "$RECORDS/disk.json"
}

# ---------------------------------------------------------------- 5. descriptor limit
limits() {
  local conf=/etc/security/limits.d/cas-experiment.conf
  if ! grep -qs "^$USER_NAME" "$conf" 2>/dev/null; then
    printf '%s soft nofile 65536\n%s hard nofile 65536\n' "$USER_NAME" "$USER_NAME" | as_root tee "$conf"
  fi
  printf '{"conf":"%s","content":"%s","current_soft":"%s","current_hard":"%s","note":"new sessions get the configured limit; the two-host agent also raises its own soft limit to 8192"}\n' \
    "$conf" "$(tr '\n' ';' < "$conf")" "$(ulimit -n)" "$(ulimit -Hn)" > "$RECORDS/ulimit.json"
}

# ---------------------------------------------------------------- 6. IRQ affinity
irqs() {
  [[ -n $NIC && -n $IRQ_CPUS ]] || { echo "--nic and --irq-cpus are required (or --skip-irq)"; return 2; }
  [[ -d /sys/class/net/$NIC ]] || { echo "no such interface $NIC"; return 1; }
  local irqs cpus n=0 before after
  irqs=$(ls "/sys/class/net/$NIC/device/msi_irqs" 2>/dev/null || grep -E "\b$NIC\b" /proc/interrupts | cut -d: -f1 | tr -d ' ')
  [[ -n $irqs ]] || { echo "no IRQs found for $NIC"; return 1; }
  mapfile -t cpus < <(python3 -c "import sys; sys.path.insert(0, '$HERE'); import campaignlib; print('\n'.join(map(str, campaignlib.parse_cpuset('$IRQ_CPUS'))))")
  if systemctl is-active --quiet irqbalance; then
    as_root systemctl stop irqbalance; echo "irqbalance stopped (not disabled; a reboot restores it)"
  fi
  before=$(for i in $irqs; do printf '%s=%s ' "$i" "$(cat /proc/irq/$i/smp_affinity_list)"; done)
  for i in $irqs; do
    echo "${cpus[$((n % ${#cpus[@]}))]}" | as_root tee "/proc/irq/$i/smp_affinity_list" >/dev/null
    n=$((n + 1))
  done
  after=$(for i in $irqs; do printf '%s=%s ' "$i" "$(cat /proc/irq/$i/smp_affinity_list)"; done)
  printf '{"nic":"%s","cpus":"%s","irqs":"%s","before":"%s","after":"%s"}\n' "$NIC" "$IRQ_CPUS" "$(echo $irqs | tr ' ' ',')" "$before" "$after" > "$RECORDS/irq.json"
}

# ---------------------------------------------------------------- 6b. SMT off: the pinning sets are physical cores
smt_off() {
  local control=/sys/devices/system/cpu/smt/control before after grub=/etc/default/grub.d/99-cas-nosmt.cfg persisted=none
  before=$(cat $control 2>/dev/null || echo unavailable)
  if [[ $before == on ]]; then echo off | as_root tee $control >/dev/null; fi
  after=$(cat $control 2>/dev/null || echo unavailable)
  case "$after" in off|forceoff|notsupported|notimplemented) ;; *) echo "SMT still $after"; return 1 ;; esac
  if [[ -d /etc/default/grub.d ]] && command -v update-grub >/dev/null; then
    if ! grep -qs nosmt /proc/cmdline && ! [[ -f $grub ]]; then
      printf 'GRUB_CMDLINE_LINUX="$GRUB_CMDLINE_LINUX nosmt"\n' | as_root tee $grub >/dev/null
      as_root update-grub >/dev/null 2>&1 && persisted=$grub || persisted="update-grub failed"
    else
      persisted="already (cmdline or $grub)"
    fi
  fi
  printf '{"before":"%s","after":"%s","cmdline_nosmt":%s,"persisted":"%s","online_cpus":%s}\n' "$before" "$after" "$(grep -qs nosmt /proc/cmdline && echo true || echo false)" "$persisted" "$(nproc)" > "$RECORDS/smt.json"
}

# ---------------------------------------------------------------- 7. inventory
inventory() {
  record interrupts-final cat /proc/interrupts
  record irq-affinity-final bash -c 'for i in /proc/irq/[0-9]*; do printf "%s %s\n" "$(basename $i)" "$(cat $i/smp_affinity_list 2>/dev/null)"; done'
  record mounts-final findmnt -J -o TARGET,SOURCE,FSTYPE,OPTIONS
  python3 "$HERE/inventory.py" --records "$RECORDS" --out "$OUT"
  echo "inventory $OUT sha256 $(hash_of "$OUT")"
}

log "setup.sh start inventory_only=$INVENTORY_ONLY device=$DEVICE mount=$MOUNT nic=$NIC records=$RECORDS"
step hardware hardware
if [[ $INVENTORY_ONLY == 0 ]]; then
  [[ $SKIP_NIX == 1 ]] || step nix install_nix
  if [[ $SKIP_BUILD == 1 ]]; then log "build skipped"; else step checkout checkout; step build build; fi
  if [[ $SKIP_DISK == 1 ]]; then log "disk skipped"; else step disk disk; fi
  step limits limits
  if [[ $SKIP_IRQ == 1 ]]; then log "irq skipped"; else step irq irqs; fi
  if [[ $SKIP_SMT == 1 ]]; then log "smt skipped"; else step smt smt_off; fi
fi
step inventory inventory
log "setup.sh done"
