# Two-host guest and storage experiments

`nix build .#two-host` builds `cas-two-host`, an SSH coordinator and host agent
using the same `cas-host`, `cas-fleet`, and pinned QEMU guests on both nodes.
The agent owns its children; EOF, interruption, and errors terminate its process
groups. Storage and evidence paths must be new. Each storage parent must be an
already-mounted, dedicated XFS filesystem. Evidence and Unix sockets stay outside
it. No physical device is formatted by the runner. Completed cases remove only
their own newly created storage directories so the next case has an exclusive
filesystem; failed case storage is retained. Logs and results are always retained.

## Guest handoff

The chosen migration is **clean shutdown, fenced backend migration, and cold boot
on B**, followed by data verification and a durable application epoch increment.
The pinned disposable NixOS boot closure is preinstalled on both hosts; the
measured backing image is the guest ext4 data disk. This preserves that
disk/workload state, not CPU/RAM, mutable OS-root state, or running processes.
Boot-closure distribution is outside the measured transfer. QEMU
live migration would additionally require validated vhost-user device and
inflight-state migration, which this backend does not implement. Guest shutdown
unmounts ext4 and issues FLUSH before QEMU exits; `cas-fleet migrate` still performs
the persistent writer-generation handoff. The daemon has one-shot image
attachments; after both guests exit, the harness restarts both daemons in cold
recovery mode before handing off. Recovery is included in `handoff_ns` and also
reported as nested `cold_recovery_ns`. The source restarts without frontend
listeners; B declares only the migrating image. Flow counter deltas start after
recovery. The old guest is gone before B boots.

Downtime starts immediately before asking A to shut down and ends after B has
booted, mounted the disk, verified the original file and acknowledged drift file,
and durably advanced the application's epoch. It includes controller RPC/SSH
costs. `stop_ns`, `handoff_ns` (or `copy_ns`), and `boot_verify_continue_ns` add to
`downtime_ns`. The daemon's own additive freeze/prepare/fence/transfer/install/
finalize times remain nested under `daemon`. These are deliberately distinct
from the guest blackout.

Provisioning uses `fsfreeze` around the manifest cut; B boots a separate identity
and verifies the file checksum. The original is thawed in a `finally` block.
Both guests independently create the same seeded drift file, wait for compaction,
and synchronize their stores. The measurement includes **background shipping and
explicit sync separately**, starting before either writer. It does not mistake
an already-repaired fleet for zero-cost synchronization. This is a reproducible
synthetic drift workload, not a replay of package updates. Each explicit fleet
operation also records `controller_elapsed_ns`, including CLI/RPC startup, so
explicit sync duration stays separate from drift-and-background-repair time.

The baseline uses raw virtio-blk images, direct host IO, and real `rsync
--whole-file --ignore-times --no-compress --fsync --stats` transfers, including an
existing drifted destination for the full-image sync control. It deliberately
forces every logical image byte onto the transfer stream, including zero ranges;
it is a **full-image copy control**, not a claim about optimal rsync deltas or
allocated-block ZFS send. It uses the same guest seed, filesystem size, shutdown,
boot, checksum and continuation checks. Exact rsync sent+received statistics are
retained. ZFS send/receive is not implemented; it requires a separate baseline on
ZFS pools. The independent-store capacity arm uses two actual independent CAS
stores; it must not be presented as two ZFS pools or a DDT measurement.

## Measurement cases

The default run executes:

- `flows-k1`, `flows-k2`: provision, independently drift, sync, sweep, capacity,
  verified cold direct reads, and guest migration. File allocation and chunk/index
  occupancy are both recorded. Fenced roots, manifests, WAL and filesystem
  overhead are not silently subtracted from physical capacity.
- `reads-memory`, `reads-storage`: replicated content on both members, forced
  remote settled reads on B, reader cache fills disabled, owner cache warmed or
  disabled respectively. WAL reads retain their normal precedence. Every fio read
  verifies CRC32C. Quiet memory/storage labels require positive server counters
  and zero service from the opposite source; otherwise the run fails.
- QD1 4 KiB random reads and sequential 4 KiB reads at configured depths. Each
  repetition has a matched GET-under-compaction-shipping cohort. The writer runs
  longer than the reader; sampled intervals must show both acknowledged PUT chunks and
  verified GET progress. Contention records the **actual** owner memory/store
  mix, since compaction may evict a warmed working set. Separate sockets do not
  imply immunity to shared CPU, cache, IO scheduler, or NIC contention.
- `raw-rsync`: direct raw-image local-media latency and sequential throughput,
  full-image provisioning, guest migration, full-image resync, and file allocation.
- `independent-stores`: two non-networked CAS stores with the same seeded data and
  drift files, separately formatted ext4 metadata, and measured allocation/index
  occupancy. The workload and file sizes match; fio write-timestamp headers and filesystem
  UUIDs/format metadata differ. This is a workload-matched capacity control, not
  a claim that independently formatted disks have identical hashes.

`settings.seconds` and `repeats` default to five 30-second runs. `depths` defaults
to 1, 4, 16, 64. Set `inline_us` to 0 for the blocking control or 50 for the bounded
reactor polling arm. The 8 MiB default working set fits the 64 MiB owner cache;
increase the storage working set separately when measuring sustained media
throughput. Queue depth requests are retained alongside fio's achieved depth
histogram; the guest queue is 128 entries. This runner does not claim to implement
prefetch, RDMA, nvmet, null-target probes, or the optional fleet-class FLUSH.

`results.json` contains schema version, exact source revision, Nix source path,
executable SHA256 hashes, configuration, hardware inventory, fio JSON+ latency
histograms, flow bytes, raw recent remote timestamps, capacity and success/error.
`recent_phases` retains the final bounded ring (up to 1,024 reads), not an unbiased
whole-run latency sample. Use fio's full-run distribution for p50/p99/p99.9 and
use the ring only for daemon phase diagnosis. Timings within each read use the
same host-local monotonic clock. Cross-host wall clocks are never subtracted.

Daemon flow bytes count each sender once (`A.sent + B.sent`). GET counters,
controller traffic and rsync statistics remain separate. These are application
bytes; SSH encryption, TCP/IP framing and retransmissions are not included.
`put_acked_chunks` counts chunks only after a successful durable PUT ACK; HAS
inventory bytes cannot satisfy the overlap check. A lost ACK may leave durable
chunks uncounted. Per-host raw command logs and RPC responses live under the configured outputs and
the coordinator output. Errors fail the run and preserve partial evidence.

## Run on two CloudLab nodes

Use the same source revision on both nodes, Nix with flakes, working `/dev/kvm`,
SSH from A to B (including a preverified host key), and an experiment account that
can write an exclusive XFS mount on local NVMe. Allow the configured GET and bulk
ports only on the isolated experiment link. Reserve enough space for the writer
cohort (at least 16 GiB per store for the default small run). Source and results
live on the OS filesystem. No command below partitions or formats a disk.

On **each** node in the source checkout:

```sh
nix build .#two-host --out-link result-two-host --cores 4
```

On **A**, edit the checked-in example: B's SSH target and absolute wrapper path,
private NIC addresses, CPU sets, fresh storage and output paths. The `runner` path
must exist on B; the local A agent uses the invoking build automatically.

```sh
cp experiments/two-host/cloudlab.example.json /var/tmp/fleet-001.json
$EDITOR /var/tmp/fleet-001.json
./result-two-host/bin/cas-two-host \
  --config /var/tmp/fleet-001.json --output /var/tmp/fleet-controller-001
```

To repeat with the blocking path, use new paths on **both** nodes and a new
coordinator output, set `settings.inline_us` to 0, and run the same command. The
runner rejects revision mismatch. Build from a clean committed tree for paper
runs; dirty development runs record the Nix source path as well as `dirtyRev`.
Archive the coordinator directory and both host output directories before reuse.

Before interpreting hardware latency, separately record/verify NIC firmware,
100 GbE link speed, MTU, IRQ affinity, moderation, CPU frequency/C-states, drive
firmware and placement, and competing processes; run TCP ping-pong and the RDMA
floor if those comparison rows are needed. The automatic inventory captures
kernel/CPU/block topology/link state and interrupts, not all firmware and switch
settings. Pin CPU/IRQ policy on the dedicated testbed, repeat and report spread.
Do not label a virtual disk or page-cache effect as NVMe service latency.

## Unprivileged local validation

The local fixture runs two actual production daemons with **separate XFS virtual
disks** inside an outer KVM VM and boots workload QEMUs using TCG. This works on
Spark without loop mounts, sudo, or nested KVM. The same coordinator and agents
run over loopback, including the rsync copy. Its numbers are development evidence
only, especially guest boot downtime and media latency.

```sh
nix build .#two-host-fixture --out-link result-two-host-fixture --cores 3
./result-two-host-fixture/bin/cas-two-host-fixture --output "$PWD/results/two-host/local-001"
uv run --no-project python -m unittest discover -s experiments/two-host -v
CARGO_BUILD_JOBS=3 just check
```

A Rust two-process regression additionally stalls a real compaction PUT before
insertion/ACK and requires GETs on the independent socket to complete with
verified data. It uses a deadline for liveness, not an unstable latency bound.

The shipping writer is rate-limited by `settings.ship_rate_bytes` (16 MiB/s by
default), with its achieved rate retained in fio output. Set it to the measured
page-03 ship rate for a matched hardware run. Leave enough store headroom for
`rate × (seconds + 10)` between sweeps. The local rsync baseline uses an owned
loopback-only rsync daemon, so it also crosses TCP rather than substituting a
local file copy.

Agents raise their descriptor soft limit to at least 8,192 within the existing
hard limit, and record the result. The pinned guests use two vCPUs, 1 GiB RAM,
a restricted QEMU user network with static control addressing, and no package
manager service. Actual QEMU argv and live KVM VM descriptors are retained and
checked against the selected KVM/TCG build. Quiet and shipping read cohorts use
the same counter-sampling schedule.

Both launchers accept `--cases flows-k1,flows-k2` (or other listed case names)
for a focused development rerun. The selected list is retained in the raw
configuration; a subset's success does not certify the omitted cases.

The local TCG fixture allows 20 minutes for guest boot and four hours for its
whole suite; native KVM guest boot retains a four-minute limit. Agent RPC waits
cover boot plus authenticated readiness. These startup limits do not alter CAS
IO or peer protocol deadlines. `host-pressure.jsonl` samples the physical
launcher's CPU, IO and memory pressure and load every five seconds, since guest
load alone cannot reveal competing work on Spark. The isolated guest explicitly
loads its virtio network driver and configures its one interface before sshd.
