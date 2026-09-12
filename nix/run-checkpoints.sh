# Host runner for the source-bound checkpoint suite.
# Nix sets CAS_HARNESS and CAS_BUILD_INFO via runtimeEnv.

exec "$CAS_HARNESS" suite --build-info "$CAS_BUILD_INFO" "$@"
