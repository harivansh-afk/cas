# Outer-guest workload for the shared-host fixture. Runs on the XFS scratch fs
# and invokes cas-harness shared. Nix sets CAS_HARNESS and CAS_BUILD_INFO via
# runtimeEnv.

exec > /results/workload.log 2>&1
findmnt --json /fixture > /results/mount.json
xfs_info /fixture > /results/xfs-info.log
uname -a > /results/kernel.log
df -B1 /fixture > /results/space-before.log
"$CAS_HARNESS" shared --root /fixture/store --output /results/shared --build-info "$CAS_BUILD_INFO" --scenario /results/scenario.json
df -B1 /fixture > /results/space-after.log
sync -f /fixture
