# Multi-host guest and storage experiments

`nix build .#two-host` builds `cas-two-host`, an SSH coordinator and host agent
using the same `cas-host`, `cas-fleet`, and pinned QEMU guests on every node.
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
boot, checksum and continuation checks; resynchronization also boots and verifies
the destination clone. Exact rsync sent+received statistics are
retained. The `zfs-incremental` arm uses real `zfs send -i` / `zfs receive -F`
against a **common snapshot with a verified matching GUID**. Its initial full
stream is recorded as `common_base_setup`, separately from incremental provision,
drift synchronization and migration. Report that setup cost alongside marginal
operation bytes; it is not free placement. Each destination is a newly created,
owned child of the configured existing `zfs_parent`; only that child may be rolled
back or destroyed. Receiving requires stopped destination guests. Source snapshots
use clean shutdown or guest fsfreeze. All arms use the same data/drift workload.

The ZFS stream crosses its own TCP connection. SHA256 and exact stream length are
checked end to end, and completion requires successful `zfs receive` plus a
matching received snapshot GUID. Counts include the stream, its one-use handshake
and receiver ACK. Per-stream commands and full setup bytes are retained. ZFS uses
4 KiB records, compression off and retained snapshots; its allocation properties
are recorded explicitly. This is incremental block transfer, not a simulated
hash-difference estimate. See the [OpenZFS send documentation](https://openzfs.github.io/openzfs-docs/man/master/8/zfs-send.8.html).

The independent-store capacity arm uses N actual independent CAS
stores; it must not be presented as two ZFS pools or a DDT measurement.

## Measurement cases

The runner accepts 2–16 static members (N=2 and N=4 are the validation targets).
The default run executes:

- `flows-k1`, `flows-k2`: provision, independently drift, sync, sweep, capacity,
  verified cold direct reads, and guest migration. File allocation and chunk/index
  occupancy are both recorded. Fenced roots, manifests, WAL and filesystem
  overhead are not silently subtracted from physical capacity.
- `reads-memory`, `reads-storage`: replicated content on every member (k=N), forced
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
- `zfs-incremental`: common-base setup and incremental provisioning, drift sync,
  verified guest migration, downtime and actual stream bytes.
- `independent-stores`: N non-networked CAS stores with the same seeded data and
  drift files, separately formatted ext4 metadata, and measured allocation/index
  occupancy. The workload and file sizes match; fio write-timestamp headers and filesystem
  UUIDs/format metadata differ. This is a workload-matched capacity control, not
  a claim that independently formatted disks have identical hashes.

Fleet capacity is sampled after coordinated sweep; the independent-store control
records settled allocation without a standalone collection pass. These are
operational footprints at the recorded checkpoints, not normalized minimum-live-data
capacity bounds. Retain the per-file breakdown when comparing them. Initialization
requires an image on every root, so fleet storage-only members retain empty
bootstrap image metadata/WALs in addition to the two workload guests. Those bytes
are included, not silently subtracted. The page-03 matched guest-count comparison
uses N=2: the fleet flow always runs two workload guests, while the independent
control runs N. N=4 allocation totals describe placement and retention overhead;
they are not a matched four-guest deduplication ratio.

`settings.seconds` and `repeats` default to five 30-second runs. `depths` defaults
to 1, 4, 16, 64. Set `inline_us` to 0 for the blocking control or 50 for the bounded
reactor polling arm. The 8 MiB default working set fits the 64 MiB owner cache;
increase the storage working set separately when measuring sustained media
throughput. Queue depth requests are retained alongside fio's achieved depth
histogram; the guest queue is 128 entries. This runner does not claim to implement
prefetch, RDMA, nvmet, null-target probes, or the optional fleet-class FLUSH.

## Multi-guest, burst and resume arms

These arms are harness additions on the frozen measured daemon revision; they
drive everything from the JSON configuration and fail instead of mislabelling.

`settings.guest` sizes every workload guest (`{"cores": 2, "memory_mib": 1024}`
is the pinned build default). Other sizes append `-m`, `-smp` and a matching
shared memfd backend after the pinned run script; QEMU takes the last value and
the harness checks the live QEMU command line against the request before any
measurement. The effective sizing is retained in each guest's `qemu.json`.
`burst.guest` and `resume.guest` override it per case.

`settings.guests_per_host` is a list of concurrent workload guest counts for
`reads-memory` and `reads-storage` (default `[1]`, maximum 16). Each entry is a
separate arm with its own storage: the one-guest arm keeps the historical case
name and result schema; larger arms are named `reads-memory-g4` and so on, and
may also be requested directly in `cases`. Each arm boots N sources on A (its
own image and seed each) and N provisioned readers on B, warms every image,
then runs each fio cohort on all N readers at once. Cohorts record the
aggregate (`fio`: summed IOPS/bytes, merged json+ latency histograms, bucket
percentiles) and every guest's complete fio JSON under `fio.guests`, together
with the owner's GET counters and the reader's recent-ring phases. The
contention cohort runs one shipping writer per source with the requested rate
divided by N, so the aggregate requested rate matches the one-guest arm.
`cache_bytes` must hold `2 × data_bytes × max(guests_per_host)`.

`burst-images` (`settings.burst`): A boots `images` guests, each writing an
identical seeded `base` region (`base_bytes`) and a per-image `unique` region
(`unique_bytes`) whose first 4 KiB block is a marker; after each image the
harness FLUSHes, waits for compaction and records the store chunk delta. All
manifests are provisioned to B under fsfreeze with content already placed
(`k`, default 1). A's guests stop, B's daemon restarts cold with an empty clean
cache (optionally `drop_page_cache`, which needs root and otherwise fails), and
all N guests launch back to back on B. Per guest it records launch→ready,
launch→first SSH command, launch→marker read (direct read of the unique
region's first block, checked against the expected marker), the guest's
`/sys/block/vda/stat` reads and used ext4 blocks, then a full checksum of both
regions. Per member it records flow bytes, every GET/HAS counter delta, GET wire
bytes, `max_inflight` before/after and B's phase ring, for the window up to the
last marker read and up to the end of verification. The daemon's GET counters
are host wide, so `fetched_fraction_of_used` is B's GET delta over the sum of
used blocks, and per-image `read_fraction_at_marker` is guest-observed device
reads over that image's used blocks; the two agree only under `force_remote`,
which needs `k ≥ 2` so every chunk has a nonlocal owner. A window with zero
GETs fails the case.

`resume-elsewhere` (`settings.resume`): one guest on A writes `setup_bytes`
of seeded data (its first block is a marker), runs the optional `setup_script`,
optionally settles and synchronizes placement, then shuts down cleanly. Both
daemons restart cold (B with an empty cache), the fenced migration runs, and B
boots the image, recording launch→first command, launch→marker read, the usual
downtime decomposition, and bytes per phase: manifest bytes and staging-tail
bytes from the daemon reply, migration flow bytes, then the missing-chunk bytes
observed while booting and verifying (ship/sync flow bytes, GET counters and
wire bytes), followed by an explicit `sync` on B whose bytes are kept separate.
`raw-rsync` and `zfs-incremental` are unchanged controls.

### Working set, boot and first useful IO

`settings.working_set` is a JSON list of guest shell command strings (for
example the bash commands of a recorded agent trajectory; the harness knows
nothing about their origin). `burst.working_set` and `resume.working_set`
override it per case; an empty list disables the phase and leaves
`working_set` null. After the marker read, every guest runs the list in order
with `sh -eu -c` from `/mnt/cas`; a failing command fails the case. Each
command records its start and completion on host B's monotonic clock, its
stdout size and SHA-256 (stdout itself is retained under the guest's output
directory as `working-set-<pass>-<index>.out`). The daemon report is taken on
every member before and after the whole list, so `cold.peer[1].gets` — B's
GET delta — is the **working set**: the chunks the replayed commands actually
fetched, with `cold.wire` and `destination_get_wire` as its bytes. The same
list then runs again immediately as the `warm` pass, a control for how much of
the cold time was storage rather than the commands themselves. In
`burst-images` every guest replays the list at once, so the counters are the
burst's aggregate and the per-guest numbers are timings only; the burst's
`windows.to_verified` then also spans the replay. The replay
runs **before** the full checksum verification, which would otherwise pull the
whole disk through the guest page cache first.

The `intervals` block holds the paper's storage-path metrics. Everything in it
is on host B's monotonic clock; cross-host clocks are never subtracted. For
`resume-elsewhere` the anchor is `handoff_b.after_ns`, B's clock read by B's
agent immediately after `cas-fleet migrate` returned to the controller; it is
late by one controller→B RPC (`handoff_bracket_ns` is the before/after width
of that bracket). For `burst-images` the anchor is the first QEMU launch.

- `launch_delay_ns`: anchor → QEMU `execve` on B. Controller RPC, result
  directory creation and the wait for the daemon's vhost socket.
- `boot_ns`: QEMU launch → the guest's `ready` marker. Firmware, kernel and
  systemd of the preinstalled boot closure, then `mount` of the migrated ext4
  disk (so the first data-disk reads — superblock, journal, group
  descriptors — fall inside it). Excludes SSH.
- `ssh_ready_ns`, `first_command_ns`, `marker_ns`: launch → sshd banner,
  → first successful SSH command, → marker block read; cumulative, as in the
  per-guest burst summary. `time_to_marker_ns` is anchor → marker.
- `first_useful_io`: anchor → the first guest read that reached B's daemon
  **and needed a remote chunk**. Reads served from B's own store never enter
  the daemon's ring, so this is the first remote fetch, not the first block
  request. The ring stamps records in nanoseconds since the peer runtime
  started, so `ring_calibration` brackets that origin against B's clock after
  all windows close: direct 4 KiB reads of distinct chunks, each bracketed on
  the host clock; a read that added exactly one ring record bounds the origin
  to `[started − received_ns, completed − completed_ns]`, and the bounds are
  intersected. `ns` is the midpoint, `lower_ns`/`upper_ns` the bracket. The
  ring is sampled right after each guest's probe, after the epoch continue
  and after the working set; if the earliest sample is at full retention
  with no pre-anchor record, the first read may have been evicted and the
  value is `censored` with `ns` null. With `k` equal to the member count no
  read is remote and the value is null with a reason.
- `time_to_working_set_ns`: anchor → completion of the last replayed command
  of the cold pass (per guest in the burst). It therefore includes
  `launch_delay_ns`, `boot_ns`, SSH readiness, the marker read, the durable
  epoch increment and the replay; `cold.guests[].replay_ns` is the replay
  alone and `warm_replay_ns` (`warm.guests[].replay_ns`) the second pass.

`migration.downtime_ns` keeps its decomposition: `stop_ns` + `handoff_ns` +
`boot_verify_continue_ns`, on the controller's clock. In `resume-elsewhere`
the last term now ends when the guest has read and checked the marker block
and durably advanced the application epoch (`continue_epoch`); the full data
checksum runs after the working set and is reported as `final_verify_ns`, with
its own `final_verify_wire`/`final_verify_peer` deltas, so a configured working
set never inflates downtime. `migration.first_command_ns` and `marker_ns` are
measured from `handoff_b.after_ns` on B's clock (earlier revisions subtracted
the controller's clock, which is only valid on a loopback fixture).

The daemon attaches each image with a fixed 60-second recovery window
(`RECOVERY_TIMEOUT`), measured from attachment to the guest driver's first
queue activation. Every guest in a burst must therefore reach its virtio-blk
driver within 60 seconds of B's cold restart (and every source within 60
seconds of A's start); a slower boot ends that image service with `recovery
deadline expired` and fails the case. KVM guests boot in a few seconds;
concurrently booting TCG guests do not fit, so these arms need KVM.

Host budget: each arm needs `guest cores × N + daemon cpus + 2` hardware
threads per member; TCG fixture guests additionally burn a host thread per vCPU
while booting. The N=16 arm with the example's one-vCPU, 1 GiB guests needs at
least 16 + 4 + 2 = 22 threads and 16 GiB of guest memory per member, plus the
daemon's staging/cache budgets; two-vCPU guests need 38 threads. Pin
`guest_cpus` to a set at least as large as `cores × N` and keep the daemon's
`cpus` disjoint; the `cloudlab*.example.json` sets must be widened accordingly
on the chosen node type.

`--set KEY=JSON` overrides one settings entry on the command line (for example
`--set guests_per_host=[4]`); the effective configuration is retained in
`config.json`.

`results.json` is `schema_version` 3: version 2 plus, under `burst-images` and
`resume-elsewhere`, `working_set`, `intervals`, `ring_calibration` and (resume)
`final_verify_ns`, `migration.handoff_b` and the extra `phases.missing_chunks`
deltas; no earlier field was renamed. It contains schema version, exact source revision, Nix source path,
executable SHA256 hashes, configuration, hardware inventory, fio JSON+ latency
histograms, flow bytes, raw recent remote timestamps, capacity and success/error.
`recent_phases` retains the final bounded ring (up to 1,024 reads), not an unbiased
whole-run latency sample. Use fio's full-run distribution for p50/p99/p99.9 and
use the ring only for daemon phase diagnosis. Timings within each read use the
same host-local monotonic clock. Cross-host wall clocks are never subtracted.

Daemon flow bytes count each sender once (the sum of each member's sent bytes). GET counters,
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

The full suite additionally needs OpenZFS with compatible userspace/kernel tools,
and a dedicated existing filesystem dataset such as `tank/cas-bench` on A and B.
The agent account must be able to create, snapshot, send, receive, mount and
destroy its child datasets; use an appropriately delegated account or root on
these isolated test hosts.
The runner creates/destroys only its fresh children; it never creates a physical
pool. Allow `zfs_port` (17487 in the example) between these nodes. Incremental
receive intentionally rolls back the owned destination's independent drift to
its recorded common base before applying A's stream.

On **each** node in the source checkout:

```sh
nix build .#two-host --out-link result-two-host --cores 4
```

On **A**, edit the checked-in example: B's SSH target and absolute wrapper path,
private NIC addresses, CPU sets, fresh storage and output paths, and `zfs_parent`. The `runner` path
must exist on B; the local A agent uses the invoking build automatically.

```sh
cp experiments/two-host/cloudlab.example.json /var/tmp/fleet-001.json
$EDITOR /var/tmp/fleet-001.json
./result-two-host/bin/cas-two-host \
  --config /var/tmp/fleet-001.json --output /var/tmp/fleet-controller-001
```

To repeat with the blocking path, use new paths on **both** nodes and a new
coordinator output, set `settings.inline_us` to 0, and run the same command. The
runner rejects revision mismatch across any member. For N=4, start from
`experiments/two-host/cloudlab-four.example.json`; member order is the stable ID
order used by both GET and bulk lists. The QEMU source/destination are nodes 0/1;
all members participate in placement, repair, collection and capacity. Build from a clean committed tree for paper
runs; dirty development runs record the Nix source path as well as `dirtyRev`.
Archive the coordinator directory and every host output directory before reuse.

Before interpreting hardware latency, separately record/verify NIC firmware,
100 GbE link speed, MTU, IRQ affinity, moderation, CPU frequency/C-states, drive
firmware and placement, and competing processes; run TCP ping-pong and the RDMA
floor if those comparison rows are needed. The automatic inventory captures
kernel/CPU/block topology/link state and interrupts, not all firmware and switch
settings. Pin CPU/IRQ policy on the dedicated testbed, repeat and report spread.
Do not label a virtual disk or page-cache effect as NVMe service latency.

## Legacy unprivileged TCG fixture

The local fixture runs two actual production daemons with **separate XFS virtual
disks** inside an outer KVM VM and boots workload QEMUs using TCG. This is available on isolated
hosts without loop mounts, sudo, or nested KVM; do not run it on shared Spark. The same coordinator and agents
run over loopback, including the rsync copy. This XFS-only fixture explicitly
excludes `zfs-incremental`; the KVM appliance validates that arm. Its numbers are development evidence
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
for a focused development rerun, and `--set KEY=JSON` settings overrides. The
selected list is retained in the raw configuration; a subset's success does
not certify the omitted cases. The fixture configuration also carries four
burst images (4 MiB base, 1 MiB unique), a 2 MiB resume setup and a
three-command working set (two direct reads of distinct regions, then a whole
file checksum), but its TCG default list excludes the multi-guest, burst and
resume arms: see the daemon window note above.

The local TCG fixture allows 20 minutes for guest boot and four hours for its
whole suite; native KVM guest boot retains a four-minute limit. Agent RPC waits
cover boot plus authenticated readiness. These startup limits do not alter CAS
IO or peer protocol deadlines. `host-pressure.jsonl` samples the physical
launcher's CPU, IO and memory pressure and load every five seconds, since guest
load alone cannot reveal competing work on Spark. The isolated guest explicitly
loads its virtio network driver and configures its one interface before sshd.

## Freestyle validation

Do not run builds, tests or fixtures on shared Spark. The persistent `cas-build`
VM retains Nix/Cargo caches; disposable x86_64 VMs run the experiments. Transfer a
git bundle or the worktree, build there, and record both source revision and any
uncommitted patch. The account currently rejects more than 8 vCPUs or 64 GiB disk
per VM; do not confuse that limit with available credits. Existing unrelated VMs
must remain untouched.

Freestyle's kernel exposes nested KVM but has no loadable ZFS module. The optional
`nix build .#fleet-appliance` produces an owned KVM NixOS host with a disposable
XFS disk and ZFS pool. It runs on a Freestyle VM and serves SSH on 17422 and the ZFS
stream on 17487, bound to `--bind`. Its inner workload guests also request KVM.
The standard runner is unchanged; configure its SSH target to this appliance,
`storage` beneath `/fleet`, `output` beneath `/results`, `zfs_parent=cas-zfs/runs`,
`zfs_bind=0.0.0.0`, and `zfs_port=17487`. Use fresh paths. Example launcher:

```sh
./result-appliance/bin/cas-fleet-appliance \
  --output /root/appliance-001 --authorized-key /root/controller.pub \
  --bind 10.172.61.2
```

Run `--cases zfs-incremental` against two such hosts. Appliance networking adds
QEMU user-network forwarding; it validates the baseline, not a physical network
latency claim. Native Freestyle hosts run the CAS multi-VM cases against separate
XFS scratch mounts. CloudLab runs all arms natively on its configured filesystems.
