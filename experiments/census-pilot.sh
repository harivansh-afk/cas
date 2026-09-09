# Run through `nix run .#census-pilot -- NEW_OUTPUT_DIRECTORY`.
# Unbooted Ubuntu ARM64 root filesystems: a dated-image pilot, not an update fleet.
set -euo pipefail
export LC_ALL=C
if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo 'usage: cas-census-pilot NEW_OUTPUT_DIRECTORY [INPUT_DIRECTORY]' >&2
  exit 2
fi
inputs=${2:-}
if [[ -n "$inputs" ]]; then inputs=$(realpath -- "$inputs"); fi
mkdir -p -- "$(dirname -- "$1")"
mkdir -- "$1"
cd -- "$1"
exec > >(tee session.log) 2>&1
date -u --iso-8601=seconds > started.txt
uname -a > host.txt
qemu-img --version > tools.txt
dumpe2fs -V >> tools.txt 2>&1

while read -r date sha; do
  url="https://cloud-images.ubuntu.com/noble/$date"
  image=noble-server-cloudimg-arm64.img
  printf '%s %s %s\n' "$date" "$sha" "$url/$image" >> sources.txt
  if [[ -n "$inputs" ]]; then
    cp --reflink=auto -- "$inputs/$date.qcow2" "$date.qcow2"
  else
    curl --fail --location --retry 2 --connect-timeout 15 --max-time 110 \
      --output "$date.qcow2" "$url/$image"
  fi
  echo "$sha  $date.qcow2" | sha256sum --check
  curl --fail --location --retry 2 --max-time 30 --output "$date.manifest" "$url/noble-server-cloudimg-arm64.manifest"
  qemu-img info --output=json "$date.qcow2" > "$date.qcow2.json"
  qemu-img convert -f qcow2 -O raw "$date.qcow2" "$date.raw"
  cas-normalize-root "$date.raw" "$date"
  chmod a-w "$date.qcow2" "$date.raw" "$date.root.raw"
done <<'IMAGES'
20260705 7df0201546f75b8bcc1044594c806c35749421ad3c9bc1be2a3ab806cfae39cc
20260826 afa139bac6f2629c1e1f2f8f34215f3a9ad9779801bcb945521ba1a45016743f
IMAGES

sha256sum ./*.raw ./*.qcow2 > SHA256SUMS
casctl census 20260705.root.raw 20260826.root.raw > census.json
# Repeating the same immutable file is the known-identical clone control.
casctl census 20260705.root.raw 20260705.root.raw > clone-control.json
jq -e 'all(.results[];
  .cross_image_duplicate_bytes == .fleet_unique_bytes and
  .images[1].base_in_place_bytes == .images[0].nonzero_chunk_bytes and
  .images[1].new_unique_bytes == 0)' clone-control.json
sha256sum --check SHA256SUMS
date -u --iso-8601=seconds > finished.txt
