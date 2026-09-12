# Host runner for the census update fleet.
# Nix sets CAS_FIRMWARE, CAS_FIRMWARE_VARS, and CAS_WORKLOAD via runtimeEnv.

exec cas-harness fleet \
  --firmware "$CAS_FIRMWARE" \
  --firmware-vars "$CAS_FIRMWARE_VARS" \
  --workload "$CAS_WORKLOAD" "$@"
