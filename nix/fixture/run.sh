# Host runner for the XFS / shared fixtures.
# Nix sets CAS_HARNESS and CAS_BUILD_INFO via runtimeEnv.

exec "$CAS_HARNESS" fixture --build-info "$CAS_BUILD_INFO" "$@"
