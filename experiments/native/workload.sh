#!/usr/bin/env bash
# Paired pilot workloads. Invoked by cas-native-run; results stay outside storage.
set -euo pipefail
mode=${1:-smoke}
seconds=${2:-30}
size_mib=${3:-512}
[[ $seconds =~ ^[0-9]+$ && $seconds -ge 1 && $seconds -le 60 ]]
[[ $size_mib =~ ^[0-9]+$ && $size_mib -ge 64 && $size_mib -le 2048 ]]
case "$mode" in smoke|baseline|pressure|accounting) ;; *) exit 2 ;; esac
printf '{"mode":"%s","seconds":%s,"size_mib":%s,"seed":240924,"direct":true,"cache_reset":false}\n' \
  "$mode" "$seconds" "$size_mib" > "$CAS_OUTPUT/workload-settings.json"

guest() {
  local vm=$1 guest_command
  shift
  printf -v guest_command '%q ' "$@"
  ssh -F "$CAS_SSH_CONFIG" "$vm" "$guest_command"
}
phase() {
  printf '%s\n' "$1" > "$CAS_OUTPUT/phase"
  date -u +%s.%N > "$CAS_OUTPUT/$1.start"
  cat "$CAS_DEVICE_STAT" > "$CAS_OUTPUT/$1.before.diskstat"
}
finish_phase() {
  cat "$CAS_DEVICE_STAT" > "$CAS_OUTPUT/$1.after.diskstat"
  date -u +%s.%N > "$CAS_OUTPUT/$1.end"
}
fio_job() {
  local vm=$1 label=$2
  shift 2
  date -u +%s.%N > "$CAS_OUTPUT/guest-${vm#vm}/$label.host-start"
  guest "$vm" timeout --signal=INT 150 fio --name="$label" \
    --filename=/mnt/cas/workload.bin --size="${size_mib}m" \
    --direct=1 --randrepeat=1 --randseed=240924 --refill_buffers=1 \
    --buffer_compress_percentage=0 --lat_percentiles=1 --percentile_list=50:99:99.9 \
    --output-format=json+ --output="/results/$label.json" "$@"
  date -u +%s.%N > "$CAS_OUTPUT/guest-${vm#vm}/$label.host-end"
  jq -e '.jobs | length > 0 and all(.[]; .error == 0)' \
    "$CAS_OUTPUT/guest-${vm#vm}/$label.json" > /dev/null
}
settle() {
  local label=$1 stable=0 deadline=$((SECONDS + 120)) sample elapsed last_elapsed
  phase "$label"
  if [[ $CAS_BACKEND == cas ]]; then
    sample=$(tail -n 2 "$CAS_OUTPUT/daemon/telemetry.jsonl")
    last_elapsed=$(jq -er '.elapsed_ns | numbers' <<< "${sample%%$'\n'*}")
    while (( SECONDS < deadline )); do
      # Read the penultimate line so an in-progress append is never mistaken for a sample.
      sample=$(tail -n 2 "$CAS_OUTPUT/daemon/telemetry.jsonl")
      elapsed=$(jq -er '.elapsed_ns | numbers' <<< "${sample%%$'\n'*}")
      if (( elapsed <= last_elapsed )); then sleep 1; continue; fi
      last_elapsed=$elapsed
      if jq -e '[.images[].report.local.status | (.published != null and .published == .compacted and .issued == .published)] | length > 0 and all' \
        <<< "${sample%%$'\n'*}" > /dev/null 2>&1; then
        stable=$((stable + 1))
        if (( stable >= 2 )); then
          finish_phase "$label"
          return 0
        fi
      else
        stable=0
      fi
      sleep 1
    done
    printf '%s\n' 'compaction did not reach the published frontier within 120 seconds' >&2
    return 1
  fi
  sleep 2
  finish_phase "$label"
}

phase prepare
for ((i=1; i<=CAS_GUESTS; i++)); do
  guest "vm$i" uname -a > "$CAS_OUTPUT/guest-$i/uname.txt"
  fio_job "vm$i" prepare --ioengine=io_uring --rw=write --bs=4k --iodepth=32 \
    --verify=crc32c --verify_interval=4096 --verify_fatal=1 --do_verify=1 --end_fsync=1
done
finish_phase prepare
settle prepare-drain

if [[ $mode == baseline ]]; then
  # Each new runner invocation is one replicate. Rotate arm order outside this script.
  for depth in 1 32; do
    label="read-q$depth"
    phase "$label"
    fio_job vm1 "$label" --ioengine=io_uring --rw=randread --bs=4k --iodepth="$depth" \
      --time_based=1 --runtime="$seconds" --ramp_time=5
    finish_phase "$label"
  done
  phase sequential-read
  fio_job vm1 sequential-read --ioengine=io_uring --rw=read --bs=128k --iodepth=32 \
    --time_based=1 --runtime="$seconds" --ramp_time=5
  finish_phase sequential-read
  for depth in 1 32; do
    label="write-q$depth"
    phase "$label"
    fio_job vm1 "$label" --ioengine=io_uring --rw=randwrite --bs=4k --iodepth="$depth" \
      --time_based=1 --runtime="$seconds" --ramp_time=5 --end_fsync=1 \
      --verify=crc32c --verify_interval=4096 --do_verify=0
    finish_phase "$label"
    settle "$label-drain"
  done
  phase flush
  fio_job vm1 flush --ioengine=psync --rw=randwrite --bs=4k --iodepth=1 --fdatasync=1 \
    --time_based=1 --runtime="$seconds" --ramp_time=5 --end_fsync=1 \
    --verify=crc32c --verify_interval=4096 --do_verify=0
  finish_phase flush
  settle flush-drain
elif [[ $mode == accounting ]]; then
  # A finite write has no ramp or timed cutoff. Device/application counters
  # cover its complete foreground work and subsequent compaction drain.
  guest vm1 cat /sys/block/vda/stat > "$CAS_OUTPUT/accounting.guest-before.diskstat"
  phase accounting
  fio_job vm1 accounting --ioengine=io_uring --rw=write --bs=4k --iodepth=32 \
    --randseed=240925 --end_fsync=1 --verify=crc32c --verify_interval=4096 --do_verify=0
  finish_phase accounting-foreground
  settle accounting-drain
  guest vm1 cat /sys/block/vda/stat > "$CAS_OUTPUT/accounting.guest-after.diskstat"
  finish_phase accounting
elif [[ $mode == pressure ]]; then
  [[ $CAS_GUESTS == 2 ]]
  phase reader-alone
  fio_job vm1 reader-alone --ioengine=io_uring --rw=randread --bs=4k --iodepth=1 \
    --time_based=1 --runtime="$seconds" --ramp_time=5
  finish_phase reader-alone
  for rate in 8 32 128 0; do
    label="mixed-${rate}m"
    rate_args=()
    if (( rate > 0 )); then rate_args+=(--rate="${rate}m"); fi
    phase "$label"
    fio_job vm2 "$label-writer" --ioengine=io_uring --rw=randwrite --bs=4k --iodepth=32 \
      --time_based=1 --runtime="$((seconds + 5))" --end_fsync=1 \
      --verify=crc32c --verify_interval=4096 --do_verify=0 "${rate_args[@]}" &
    writer=$!
    sleep 2
    fio_job vm1 "$label-reader" --ioengine=io_uring --rw=randread --bs=4k --iodepth=1 \
      --time_based=1 --runtime="$seconds" &
    reader=$!
    wait "$writer"
    wait "$reader"
    finish_phase "$label"
    settle "$label-drain"
  done
fi

phase verify
for ((i=1; i<=CAS_GUESTS; i++)); do
  fio_job "vm$i" verify --ioengine=io_uring --rw=read --bs=4k --iodepth=16 \
    --verify=crc32c --verify_interval=4096 --verify_fatal=1 --verify_only=1 \
    --verify_header_seed=0 --verify_write_sequence=0
done
finish_phase verify
du -B1 -s "$CAS_STORAGE" > "$CAS_OUTPUT/storage-allocated.txt"
printf '%s\n' done > "$CAS_OUTPUT/phase"
