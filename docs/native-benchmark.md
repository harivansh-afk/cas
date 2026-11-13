# Native KVM pilot

`nix build .#native --out-link result-native` builds a pinned NixOS workload
guest and `cas-native-run`. Storage runs directly on the Linux host. There is
no outer VM. Each arm boots `--guests N` (1 to 16) identical two-vCPU, 2 GiB
KVM guests, each with one virtio queue of 128 entries and its own fresh 4 GiB
data disk (`raw-<i>.img`, or one CAS image per guest in a single store). Raw
uses QEMU's direct/io_uring file backend; daemon uses the raw vhost-user
reference; CAS uses `cas-host`. Readiness allows 120 s plus 30 s per guest;
the daemon's attach window is unchanged.

This runner never formats or mounts a physical disk. Prepare an explicitly
approved, dedicated filesystem first: XFS for the CAS and daemon backends;
XFS or a ZFS dataset for the raw backend, so the ZFS control shares this
runner. The storage directory must be new; reports must be on another
filesystem. The runner checks the supplied device against the storage
filesystem (the mount's own device for XFS; for ZFS, `--device` must be a
vdev of the dataset's pool as `zpool list -v -P` reports it) and verifies a
live KVM descriptor in QEMU. The storage filesystem type is recorded in
`identity.json`. Loop devices require `--allow-loop-device` and are labelled
development runs.

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
and QD32, 128 KiB sequential reads, and write-plus-fdatasync at QD1, with
every guest running each job at the same time; one guest is the historical
pilot. `pressure` needs at least two guests: the last guest writes at 8, 32,
128 MiB/s and without a rate cap while every other guest reads at QD1 (the
`reader-alone` phase runs those readers without the writer). These are
offered rates; report achieved rates too. Per-guest fio `json+` stays under
`guest-<i>/<label>.json`; `aggregate-<label>.json` sums IOPS and bandwidth
over the guests and gives the min/max/mean of each guest's own clat p50, p99
and p99.9, not a merged histogram. `smoke` and `accounting` use guest 1.
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
Timed CRC jobs disable fio verification-state checkpoints. In fio 3.41, the
ramp resets the IO issue count but leaves that checkpoint counter unchanged,
which can abort the job. CRC generation and the final scan remain enabled.
No test here resumes fio from a saved verification state.
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
