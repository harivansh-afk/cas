# Native KVM pilot

`nix build .#native --out-link result-native` builds a pinned NixOS workload
guest and `cas-native-run`. Storage runs directly on the Linux host. There is
no outer VM. Each arm has the same two-vCPU, 2 GiB KVM guest, one virtio queue
of 128 entries, and a fresh 4 GiB data disk. Raw uses QEMU's direct/io_uring
file backend; daemon uses the raw vhost-user reference; CAS uses `cas-host`.

This runner never formats or mounts a physical disk. Prepare an explicitly
approved, dedicated XFS filesystem first. The storage directory must be new;
reports must be on another filesystem. The runner checks the supplied device
against the storage filesystem and verifies a live KVM descriptor in QEMU.
Loop devices require `--allow-loop-device` and are labelled development runs.

```sh
nix build .#native --out-link result-native
systemd-run --user --scope -p MemoryMax=8G -p MemorySwapMax=0 \
  taskset -c 0-7 result-native/bin/cas-native-run \
  --backend cas --guests 1 \
  --device /dev/disk/by-id/APPROVED_TEST_DISK \
  --storage /srv/cas-testbed/cas-rep-01 \
  --output "$PWD/results/cas-rep-01" \
  --script "$PWD/experiments/native/workload.sh" \
  --script-arg baseline --script-arg 30 --script-arg 512
```

Choose CPU IDs from the actual host topology and retain that choice for every
arm. Run `smoke` first. `baseline` measures 4 KiB random reads/writes at QD1
and QD32, 128 KiB sequential reads, and write-plus-fdatasync at QD1. `pressure`
needs `--guests 2`: one guest reads while the other writes at 8, 32, 128 MiB/s
and without a rate cap. These are offered rates; report achieved rates too.
The second script argument is timed seconds; the third is working-set MiB.
`accounting` performs one finite sequential overwrite with a new seed, without
ramp or time limit, followed by drain. It retains guest and host-device sector
counters around that whole interval. Timed-job phase counters include ramp and
drain differently from fio's reported bytes; do not use them for amplification
or CPU-per-byte ratios. Device counters measure host writes, not SSD NAND writes.

Each invocation creates fresh storage, verifies seeded data before measurement,
and checks CRCs afterward. The final independent scan disables seed/sequence
matching across separate fio jobs; it checks content/header integrity, not
latest-write freshness after a crash. Rotate arm order across at least five repetitions.
The baseline reads begin only after CAS's compaction frontier catches the
published writes in two distinct advancing samples after the drain begins.
Record drain failures and failed
jobs; do not filter them out of the cohort.

Guest IO is direct. The raw host file is direct. CAS's clean cache is 16 MiB
by default; the 512 MiB working set exceeds it. Cache state is not reset between
jobs; these are warm steady-state pilots, not cold-media measurements. Writes
carry fio CRC headers so the final verification can detect corruption. Normal
write completion and fdatasync latency are separate distributions; their p99s
cannot be added into a transaction p99. A timed write job runs background
compaction; the pressure arm measures concurrent-reader interference explicitly.

Every result contains pinned build/lock identities, exact process commands,
QEMU invocation and KVM evidence, fio JSON+ histograms and per-job wall times,
guest kernels, startup/shutdown
outcome, one-second host/process/cgroup samples, block-device counters, and
CAS telemetry. Guest memory and daemon shared mappings overlap PSS/cgroup
accounting; do not sum those views. Device-sector counters include guest
filesystem and CAS metadata traffic, not just application bytes.

No daemon code or recovery timeout is changed by this runner. A successful
pilot is not a full C5 rerun, ZFS comparison, physical-power-loss validation,
or acceptance of a paper gate. Keep analysis and complete research records in
the private research repository; publish only the reviewed result projection.

Storage and failure artifacts remain after a run. The generated `key` file is
a private guest SSH key: exclude it when exporting results. Before the host
expires, copy logs, JSON and input identities elsewhere and verify their hashes.
