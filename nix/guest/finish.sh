# ExecStopPost for cas-smoke: record how the workload ended, then power off.
# systemd provides SERVICE_RESULT, EXIT_CODE, and EXIT_STATUS.

printf '{"schema_version":1,"service_result":"%s","exit_code":"%s","exit_status":"%s"}\n' \
  "$SERVICE_RESULT" "${EXIT_CODE:-unknown}" "${EXIT_STATUS:-unknown}" > /results/completion.json
sync

# Root is disposable tmpfs. After syncing results, bypass systemd's shutdown
# ramfs and its teardown of the shared host Nix store.
systemctl --force --force poweroff
