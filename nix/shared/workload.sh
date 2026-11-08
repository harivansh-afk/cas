# Outer-guest workload for the shared-host fixture. Runs on the XFS scratch fs
# and invokes cas-harness shared. Nix sets CAS_HARNESS and CAS_BUILD_INFO via
# runtimeEnv.

exec > /results/workload.log 2>&1
findmnt --json /fixture > /results/mount.json
xfs_info /fixture > /results/xfs-info.log
uname -a > /results/kernel.log
df -B1 /fixture > /results/space-before.log
# The controller and inner guests exchange readiness and completion records.
# Keep this traffic on the outer guest's local filesystem, avoiding a second
# 9p hop in the measured burst interval. The source data path is unchanged.
"$CAS_RETAIN" /fixture/shared-results /results/shared \
  "$CAS_HARNESS" shared --root /fixture/store --output /fixture/shared-results --build-info "$CAS_BUILD_INFO" --scenario /results/scenario.json
df -B1 /fixture > /results/space-after.log
sync -f /fixture
