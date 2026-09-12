set -euo pipefail
exec > /results/workload.log 2>&1
findmnt --json /fixture > /results/mount.json
xfs_info /fixture > /results/xfs-info.log
uname -a > /results/kernel.log
df -B1 /fixture > /results/space-before.log
mkdir /fixture/tmp
export TMPDIR=/fixture/tmp
dd if=/dev/zero of=/fixture/source bs=1M count=16 conv=fdatasync status=none
cp --reflink=always /fixture/source /fixture/clone
sync -f /fixture
sha256sum /fixture/source /fixture/clone > /results/before.sha256
for file in source clone; do
  xfs_io -c 'fiemap -v' "/fixture/$file" > "/results/$file-before.fiemap"
done
printf changed | dd of=/fixture/clone conv=notrunc,fdatasync status=none
sha256sum /fixture/source /fixture/clone > /results/after.sha256
for file in source clone; do
  xfs_io -c 'fiemap -v' "/fixture/$file" > "/results/$file-after.fiemap"
done
run_tests() {
  local module="$1" executable="$2" filter="$3"
  "$executable" "$filter" --list > "/results/$module.list"
  "$executable" "$filter" --include-ignored --test-threads=1 > "/results/$module.log" 2>&1
}
export CAS_SPACE_REPORT=/results/space-observations.json
run_tests space "$CAS_CORE_TESTS" space::filesystem::tests
for scope in store:file manifest:file append:shared; do
  module="${scope%:*}"
  filter="$module::${scope#*:}::tests"
  run_tests "$module" "$CAS_CORE_TESTS" "$filter"
done
run_tests catalog "$CAS_CORE_TESTS" catalog::
run_tests runtime "$CAS_DAEMON_TESTS" local::host::tests
df -B1 /fixture > /results/space-after.log
sync -f /fixture
