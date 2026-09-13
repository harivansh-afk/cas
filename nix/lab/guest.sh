set -euo pipefail
if test -f /results/format; then
  test -z "$(blkid -s TYPE -o value /dev/vda || true)"
  mkfs.ext4 -F -b 4096 -E nodiscard,lazy_itable_init=0,lazy_journal_init=0 /dev/vda
fi
mkdir -p /mnt/cas
mount -t ext4 -o data=ordered /dev/vda /mnt/cas
findmnt --json /mnt/cas > /results/mount.json
lsblk --bytes --json > /results/disks.json
touch /results/ready
while ! test -f /results/stop; do sleep 0.5; done
sync -f /mnt/cas
umount /mnt/cas
blockdev --flushbufs /dev/vda
sync -f /results
systemctl --force --force poweroff
