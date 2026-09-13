# Host runner for cas-vm-smoke / cas-dev-vm.
# Nix sets CAS_HARNESS, CAS_VM, CAS_BUILD_INFO, and CAS_LOCK via runtimeEnv.

exec "$CAS_HARNESS" vm \
  --vm "$CAS_VM" \
  --build-info "$CAS_BUILD_INFO" \
  --lock "$CAS_LOCK" \
  "$@"
