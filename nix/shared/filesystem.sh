# Inner-guest workload for shared filesystem images. $CAS_HARNESS and $CAS_SQLITE
# are set by Nix via runtimeEnv.

exec > /results/workload.log 2>&1
phase=$(cat /results/phase)
image=$(cat /results/image)
uname -a > /results/kernel.log
lsblk --bytes --json > /results/disks.json
test "$(blockdev --getsize64 /dev/vda)" = 536870912
test "$(blockdev --getss /dev/vda)" = 4096
printf '{"max_segments":%s,"max_segment_size":%s,"max_sectors_kb":%s,"max_hw_sectors_kb":%s}\n' \
  "$(cat /sys/block/vda/queue/max_segments)" "$(cat /sys/block/vda/queue/max_segment_size)" \
  "$(cat /sys/block/vda/queue/max_sectors_kb)" "$(cat /sys/block/vda/queue/max_hw_sectors_kb)" > /results/queue-limits.json
case "$phase" in
  write) mkfs.ext4 -F -b 4096 -E nodiscard,lazy_itable_init=0,lazy_journal_init=0 /dev/vda ;;
  verify) e2fsck -fn /dev/vda ;;
  *) exit 1 ;;
esac
mkdir -p /mnt/cas
mount -t ext4 -o data=ordered /dev/vda /mnt/cas
findmnt --json /mnt/cas > /results/mount.json
tune2fs -l /dev/vda > /results/ext4.log
continuation=()
if test -f /results/continued; then continuation+=(--continued); fi
"$CAS_HARNESS" filesystem --root /mnt/cas --output /results/workload --phase "$phase" --image "$image" --sqlite "$CAS_SQLITE" "${continuation[@]}"
if test -f /results/live-recovery; then
  touch /results/ready
  while ! test -f /results/continue; do sleep 0.05; done
  "$CAS_HARNESS" filesystem --root /mnt/cas --output /results/resumed --phase resume --image "$image" --sqlite "$CAS_SQLITE"
  touch /results/updated
  while ! test -f /results/resume; do sleep 0.05; done
fi
if test "$phase" = write; then
  fstrim -v /mnt/cas > /results/trim.log
fi
umount /mnt/cas
blockdev --flushbufs /dev/vda
