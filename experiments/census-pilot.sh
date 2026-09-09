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
  sfdisk --json "$date.raw" > "$date.partitions.json"
  # Select the one Linux filesystem partition; omit EFI and boot partitions.
  read -r offset length < <(jq -er '
    .partitiontable | .sectorsize as $sector |
    [.partitions[] | select(.type == "0FC63DAF-8483-4772-8E79-3D69D8477DE4")] |
    if length == 1 then .[0] | [.start * $sector, .size * $sector] | @tsv
    else error("expected one Linux root partition") end
  ' "$date.partitions.json")
  dd if="$date.raw" of="$date.root.raw" bs=4M iflag=skip_bytes,count_bytes \
    skip="$offset" count="$length" conv=sparse status=none
  # Read the original guest allocation bitmap. Do not use e2image here: it
  # regenerates backup metadata, changing some allocated bytes.
  dumpe2fs "$date.root.raw" > "$date.allocation.txt" 2> "$date.dumpe2fs.log"
  block_size=$(awk '/^Block size:/ {print $3}' "$date.allocation.txt")
  block_count=$(awk '/^Block count:/ {print $3}' "$date.allocation.txt")
  [[ "$block_size" == 4096 && "$block_count" =~ ^[0-9]+$ ]]
  truncate --size="$((block_count * block_size))" "$date.root.raw"
  awk '/^  Free blocks:/ {
    sub(/^  Free blocks: */, ""); gsub(/,/, "");
    for (i=1; i<=NF; i++) {
      split($i, bounds, "-"); start=bounds[1]; end=(bounds[2] == "" ? start : bounds[2]);
      printf "%.0f %.0f\n", start * 4096, (end - start + 1) * 4096;
    }
  }' "$date.allocation.txt" > "$date.free-ranges.txt"
  while read -r offset length; do
    fallocate --punch-hole --offset "$offset" --length "$length" "$date.root.raw"
  done < "$date.free-ranges.txt"
  dumpe2fs -h "$date.root.raw" > "$date.ext4.txt" 2>&1
  e2fsck -fn "$date.root.raw" > "$date.fsck.txt" 2>&1
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
