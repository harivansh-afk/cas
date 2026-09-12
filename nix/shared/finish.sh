# ExecStopPost for cas-filesystem: record the outcome, keep journals, then power off.
# systemd provides SERVICE_RESULT, EXIT_CODE, and EXIT_STATUS.

printf '{"schema_version":1,"service_result":"%s","exit_code":"%s","exit_status":"%s"}\n' \
  "$SERVICE_RESULT" "$EXIT_CODE" "$EXIT_STATUS" > /results/completion.json
journalctl -u cas-filesystem.service --no-pager > /results/service.log
journalctl -k --no-pager > /results/kernel-journal.log
sync
systemctl --force --force poweroff
