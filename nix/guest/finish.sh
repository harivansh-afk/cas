# ExecStopPost for cas-smoke: record the outcome, then power off or allow SSH.
# systemd provides SERVICE_RESULT, EXIT_CODE, and EXIT_STATUS.

printf '{"schema_version":1,"service_result":"%s","exit_code":"%s","exit_status":"%s"}\n' \
  "$SERVICE_RESULT" "${EXIT_CODE:-unknown}" "${EXIT_STATUS:-unknown}" > /results/completion.tmp
mv /results/completion.tmp /results/completion.json
sync

# Development guests stay available only after a successful workload.
if [ "$1" = interactive ] && [ "$SERVICE_RESULT" = success ]; then
  exit 0
fi

# Root is disposable tmpfs. After syncing results, bypass systemd's shutdown
# ramfs and its teardown of the shared host Nix store.
systemctl --force --force poweroff
