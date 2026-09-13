# Flush the experiment disk and power off an interactive guest.

blockdev --flushbufs /dev/disk/by-id/virtio-cas-experiment
sync
systemctl --force --force poweroff
