#!/usr/bin/env bash
# Run with a built cas-dev-vm, a new evidence directory, and an optional port.
set -euo pipefail
runner=$(realpath "$1")
mkdir -p "$(dirname "$2")"
mkdir "$2"
output=$(realpath "$2")
port=${3:-23479}
keys=$(mktemp -d)
pid=
cleanup() {
  if [ -n "$pid" ]; then
    kill -TERM "$pid" 2>/dev/null || true
    wait "$pid" || true
  fi
  rm -rf "$keys"
}
trap cleanup EXIT
ssh-keygen -q -t ed25519 -N '' -f "$keys/id"
ssh-keygen -q -t ed25519 -N '' -f "$keys/other"
"$runner" --ssh-key "$keys/id.pub" --ssh-port "$port" --output "$output/session" > "$output/runner.log" 2>&1 &
pid=$!
deadline=$((SECONDS + 100))
until [ -s "$output/session/known_hosts" ]; do
  kill -0 "$pid"
  test "$SECONDS" -lt "$deadline"
  sleep 0.1
done
cd "$output/session"
ssh_options=(-F /dev/null -o BatchMode=yes -o IdentitiesOnly=yes -o IdentityAgent=none
  -o ConnectTimeout=5 -o StrictHostKeyChecking=yes
  -o UserKnownHostsFile=known_hosts -p "$port")
if ssh "${ssh_options[@]}" -i "$keys/other" root@127.0.0.1 true > "$output/rejected-key.log" 2>&1; then
  echo 'Unexpected login with an unauthorized key' >&2
  exit 1
fi
[[ $(< "$output/rejected-key.log") == *"Permission denied (publickey)"* ]]
# A valid client key must still fail when the guest host key does not match.
cp known_hosts known_hosts.saved
printf '[127.0.0.1]:%s %s\n' "$port" "$(cat "$keys/other.pub")" > known_hosts
if ssh "${ssh_options[@]}" -i "$keys/id" root@127.0.0.1 true > "$output/rejected-host.log" 2>&1; then
  echo 'Unexpected login with a mismatched host key' >&2
  exit 1
fi
[[ $(< "$output/rejected-host.log") == *"HOST IDENTIFICATION HAS CHANGED"* ]]
mv known_hosts.saved known_hosts
ss -H -ltn "sport = :$port" > "$output/listener.txt"
read -r _ _ _ address _ < "$output/listener.txt"
test "$address" = "127.0.0.1:$port"
test "$(wc -l < "$output/listener.txt")" -eq 1
ssh "${ssh_options[@]}" -i "$keys/id" root@127.0.0.1 'bash -se' > "$output/ssh.log" 2>&1 <<'GUEST'
uname -a
cat /proc/sys/kernel/random/boot_id
lsblk --bytes --output NAME,TYPE,SIZE,LOG-SEC
sshd -T > /results/sshd.txt
fio --name=ssh-manual --filename=/dev/disk/by-id/virtio-cas-experiment \
  --rw=write --bs=4k --size=4m --offset=64m --direct=1 --ioengine=libaio \
  --iodepth=8 --verify=crc32c --verify_fatal=1 --end_fsync=1 \
  --output-format=json --output=/results/manual-fio.json
journalctl -u cas-smoke -u sshd --no-pager > /results/services.log
GUEST
jq -e '.jobs | length == 1' "$output/session/guest/manual-fio.json"
jq -e '.jobs[0] | .error == 0 and .write.io_bytes == 4194304 and .read.io_bytes == 4194304' \
  "$output/session/guest/manual-fio.json"
ssh "${ssh_options[@]}" -i "$keys/id" root@127.0.0.1 \
  'nohup sh -c "sleep 1; cas-poweroff" >/dev/null 2>&1 </dev/null &'
wait "$pid"
pid=
jq -e '.passed and .artifact == "development_staging_vm_interactive" and .paper_gate == null' \
  "$output/session/summary.json"
test -z "$(ss -H -ltn "sport = :$port")"
echo 'SSH authentication, loopback binding, CAS IO, and shutdown passed.'
