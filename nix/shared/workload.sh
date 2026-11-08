# Outer-guest workload for the shared-host fixture. Runs on the XFS scratch fs
# and invokes cas-harness shared. Nix sets CAS_HARNESS and CAS_BUILD_INFO via
# runtimeEnv.

exec > /results/workload.log 2>&1
findmnt --json /fixture > /results/mount.json
xfs_info /fixture > /results/xfs-info.log
uname -a > /results/kernel.log
df -B1 /fixture > /results/space-before.log
# The controller and inner guests exchange readiness and completion records.
# Keep this traffic off both the storage filesystem and the second 9p hop.
# A dedicated bounded tmpfs preserves the host's report/storage isolation.
mkdir /run/cas-shared-control
mount -t tmpfs -o size=256M,mode=0700,nodev,nosuid tmpfs /run/cas-shared-control
findmnt --json /run/cas-shared-control > /results/control-mount.json
"$CAS_RETAIN" /run/cas-shared-control/results /results/shared \
  "$CAS_HARNESS" shared --root /fixture/store --output /run/cas-shared-control/results --build-info "$CAS_BUILD_INFO" --scenario /results/scenario.json
df -B1 /fixture > /results/space-after.log
sync -f /fixture
