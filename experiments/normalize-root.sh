# Copy one ext4 root partition and zero only guest-free blocks.
set -euo pipefail
export LC_ALL=C
[[ $# == 2 ]] || { echo 'usage: cas-normalize-root RAW_IMAGE NEW_PREFIX' >&2; exit 2; }
raw=$1
prefix=$2
[[ ! -e "$prefix.root.raw" ]] || { echo 'output already exists' >&2; exit 2; }
sfdisk --json "$raw" > "$prefix.partitions.json"
# Select the one Linux filesystem partition; omit EFI and boot partitions.
read -r offset length < <(jq -er '
  .partitiontable | .sectorsize as $sector |
  [.partitions[] | select(.type == "0FC63DAF-8483-4772-8E79-3D69D8477DE4")] |
  if length == 1 then .[0] | [.start * $sector, .size * $sector] | @tsv
  else error("expected one Linux root partition") end
' "$prefix.partitions.json")
dd if="$raw" of="$prefix.root.raw" bs=4M iflag=skip_bytes,count_bytes \
  skip="$offset" count="$length" conv=sparse status=none
# Read the original guest allocation bitmap. Do not use e2image here: it
# regenerates backup metadata, changing some allocated bytes.
dumpe2fs "$prefix.root.raw" > "$prefix.allocation.txt" 2> "$prefix.dumpe2fs.log"
block_size=$(awk '/^Block size:/ {print $3}' "$prefix.allocation.txt")
block_count=$(awk '/^Block count:/ {print $3}' "$prefix.allocation.txt")
[[ "$block_size" == 4096 && "$block_count" =~ ^[0-9]+$ ]]
truncate --size="$((block_count * block_size))" "$prefix.root.raw"
awk '/^  Free blocks:/ {
  sub(/^  Free blocks: */, ""); gsub(/,/, "");
  for (i=1; i<=NF; i++) {
    split($i, bounds, "-"); start=bounds[1]; end=(bounds[2] == "" ? start : bounds[2]);
    printf "%.0f %.0f\n", start * 4096, (end - start + 1) * 4096;
  }
}' "$prefix.allocation.txt" > "$prefix.free-ranges.txt"
while read -r offset length; do
  fallocate --punch-hole --offset "$offset" --length "$length" "$prefix.root.raw"
done < "$prefix.free-ranges.txt"
dumpe2fs -h "$prefix.root.raw" > "$prefix.ext4.txt" 2>&1
e2fsck -fn "$prefix.root.raw" > "$prefix.fsck.txt" 2>&1
chmod a-w "$prefix.root.raw"
