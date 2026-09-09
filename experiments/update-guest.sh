#!/bin/bash
# Executed only inside disposable Ubuntu guests, with evidence on a separate 9p share.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive LC_ALL=C NEEDRESTART_MODE=a
mkdir -p /mnt/evidence
mount -t 9p -o trans=virtio,version=9p2000.L cas-evidence /mnt/evidence
exec > >(tee /mnt/evidence/workload.log /dev/ttyAMA0) 2>&1
finish() {
  status=$?
  printf '%s\n' "$status" > /mnt/evidence/exit-code
  sync
  systemctl poweroff
}
trap finish EXIT
date -u --iso-8601=seconds > /mnt/evidence/started.txt
uname -a > /mnt/evidence/kernel.txt
dpkg-query -W -f='${binary:Package}\t${Version}\n' > /mnt/evidence/packages-before.tsv

snapshot=@SNAPSHOT@
role=@ROLE@
if [[ "$snapshot" != base ]]; then
  rm -f /etc/apt/sources.list /etc/apt/sources.list.d/ubuntu.sources
  cat > /etc/apt/sources.list.d/census.sources <<EOF
Types: deb
URIs: https://snapshot.ubuntu.com/ubuntu/$snapshot
Suites: noble noble-updates noble-security
Components: main universe restricted multiverse
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
Check-Valid-Until: no
EOF
  cp /etc/apt/sources.list.d/census.sources /mnt/evidence/
  apt-get -o Acquire::Retries=2 -o Acquire::https::Timeout=30 update
  cp /var/lib/apt/lists/*InRelease /mnt/evidence/
  apt-get -o APT::Get::Always-Include-Phased-Updates=true \
    -o Dpkg::Options::=--force-confold -y dist-upgrade
  case "$role" in
    web) package=nginx-light ;;
    cache) package=redis-server ;;
    database) package=postgresql-client ;;
    *) exit 2 ;;
  esac
  apt-get -o Dpkg::Options::=--force-confold -y install "$package"
  apt-get clean
  rm -rf /var/lib/apt/lists/*
  # Small, explicit per-guest drift; no fabricated shared payload is inserted.
  for request in $(seq 1 1000); do
    printf '%s %s request=%s\n' "$snapshot" "$role" "$request"
  done >> /var/log/census-workload.log
fi
dpkg-query -W -f='${binary:Package}\t${Version}\n' > /mnt/evidence/packages-after.tsv
date -u --iso-8601=seconds > /mnt/evidence/finished.txt
