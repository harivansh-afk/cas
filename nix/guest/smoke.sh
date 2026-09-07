# Guest-side workload for cas-vm-smoke. Runs once at boot as a systemd oneshot.
#
# $1 is the block backend under test: raw, daemon, or staging. Everything under
# /results is a host directory shared over 9p; the host reads it after poweroff.

backend=$1
disk=/dev/disk/by-id/virtio-cas-experiment

test -b "$disk"
test "$(blockdev --getss "$disk")" = 4096

# Record what this guest saw, alongside the jobs it ran.
cp /etc/cas/smoke.fio /results/smoke.fio
uname -a > /results/guest-kernel.txt
fio --version > /results/guest-fio-version.txt
lsblk --json --bytes --output NAME,TYPE,SIZE,LOG-SEC,PHY-SEC > /results/guest-disks.json

# Recovery runs boot the staging guest twice. The host writes the phase file
# before each boot: "write" to fill and flush, then "read" to verify after the
# daemon was killed and restarted.
if [ "$backend" = staging ] && [ -f /results/recovery-phase ]; then
  cp /etc/cas/recovery.fio /results/recovery.fio
  case "$(cat /results/recovery-phase)" in
    write)
      fio --section=recovery-write --output-format=json+ --output=/results/recovery.json /etc/cas/recovery.fio
      blockdev --flushbufs "$disk"
      printf '{"schema_version":1,"phase":"write_flushed"}\n' > /results/write-flushed.tmp
      mv /results/write-flushed.tmp /results/write-flushed.json
      # The host kills the daemon while this guest waits after FLUSH.
      sleep infinity
      ;;
    read)
      fio --section=recovery-read --output-format=json+ --output=/results/recovery.json /etc/cas/recovery.fio
      exit 0
      ;;
    *)
      exit 1
      ;;
  esac
fi

fio --output-format=json+ --output=/results/fio.json /etc/cas/smoke.fio

# The queue-depth job only means something through the vhost-user daemon.
if [ "$backend" != raw ]; then
  cp /etc/cas/queue.fio /results/queue.fio
  fio --output-format=json+ --output=/results/queue.json /etc/cas/queue.fio
fi
