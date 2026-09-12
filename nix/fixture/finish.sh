# ExecStopPost for cas-fixture: record the outcome, keep the journal, then power off.
# systemd provides SERVICE_RESULT, EXIT_CODE, and EXIT_STATUS.

printf '{"schema_version":1,"service_result":"%s","exit_code":"%s","exit_status":"%s"}\n' \
  "$SERVICE_RESULT" "$EXIT_CODE" "$EXIT_STATUS" > /results/completion.json
journalctl -u cas-fixture.service --no-pager > /results/service.log
systemctl --force --force poweroff
