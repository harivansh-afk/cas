# ix block volumes: what was built, what was measured, what it teaches

Source: `/Users/rathi/Documents/Git/indexable/ix/docs/` (block-volumes/, migration-design/,
snapshot-design/, templates-design/, vmfs/, research/, hardware/ovh, prompts/). All paths below
are relative to that directory unless they start with `crates/`, which are ix source paths the
docs cite. Design notes under `crates/storage/blockvol/design/01..09-*.md` are referenced by the
docs constantly; they exist but were not read for this report. Dates run 2026-05 (vcfs research)
through 2026-08-07 (migration PR H).

Reading key for the research design being compared against: vhost-user-blk daemon, local staging
log with fdatasync ack, background compactor into a chunk store, rendezvous placement across two
hosts with replication factor k, per-host RAM cache, cold remote reads over TCP, migration by
moving the offset-to-hash map, epoch GC.

---

## 1. How ix block volumes actually work

### 1.1 The pivot and the shape

Until 2026-07-25 ix served guests a content-addressed copy-on-write filesystem (vcfs) over a
patched guest kernel and a shared-memory channel (26 metadata opcodes, a 7,740-line kernel patch,
a per-VM `vmfsd` daemon). On 2026-07-25 Hari ruled the branch block-storage-only: a versioned
virtio-blk device, stock guest kernel (CONFIG-only, no patches ever), XFS in the guest, vcfs
frozen and later deleted, no coexistence machinery, existing VMs recreated
(`block-volumes/block-volumes-spec.md` sections 1, 1a, 4). The stated reason: the substrate
(intent log, movement engine, CAS, replication, heads, credits) "does not know what a file is",
and every user-facing bug class was POSIX semantics being reimplemented. What they gave up:
file-granular cross-VM dedup, host-side file introspection, file-level diffs (section 1, 6, 8).

The stack, end to end (spec section 3, confirmed by `snapshot-design/02-current-system-inventory.md`
section 4):

```
guest page cache -> XFS journal -> blk-mq -> virtio-blk (queue per vCPU)
  -> host backend IN the VMM process: index.apply_write mints LSN under one lock and
     appends (LBA, len, LSN, bytes) to the intent log inside the same critical section
  -> completion watermark fetch_max(lsn) BEFORE the used-ring push -> ack
guest FLUSH -> wait_for_watermark(max(through, barrier)) -> group-commit fdatasync -> ack
governor -> fold: gather latest-per-block from RAM index, coalesce, zero-detect, CDC,
     put_chunk_batch to local CAS (records outbound replica intents co-atomically),
     commit extent tree (prolly, LBA keys), write manifest blob, publish head, retire
node-agent publisher (5 s tick) -> fenced live-head UPDATE in Postgres
     -> replication watermark -> durable promote -> AdvanceCheckpoint (log truncation)
```

Reads: guest page cache -> RAM extent index overlay (read-your-writes) -> extent tree via node
cache -> chunk cache (256 MiB) -> local CAS -> peer CAS. Blocks the map references but the node
does not hold are faulted on demand ("lazy blocks"), which is also how restore and migration
targets start (spec section 3, 7).

### 1.2 Backend placement: in-process, deliberately, with a known cost

The backend lives inside `vmm-worker` as library tasks behind the virtio device. The per-VM
`vmfsd` daemon, its control socket and its sidecar cgroup were deleted in #8750 and "are not
coming back" (`block-volumes/volume-backends.md` section 1, 2). Host-side control (drain, cut
export, checkpoint advance) flows over the VMM's existing control-socket RPCs.

The cost is stated plainly: a backend crash kills the VM, so the crash guarantee is "zero acked
loss via log replay", not "restart-transparent" (spec section 6, G3 caveat). The fault test had
to inject stalls through a file seam (`IX_BLOCKVOL_STALL_FILE`) because SIGKILL on the backend is
SIGKILL on the guest (`block-volumes/WS-VMM-REPORT.md`, "The design-03 fault test").

Rule that follows from it: no Postgres in the VMM worker, ever. CAS access is in the worker,
Postgres is in the node-agent, so an escaped guest lands in a process with no control-plane
credentials (`snapshot-design/02-current-system-inventory.md` invariants table). This rule later
forced the migration cutover onto a separate orchestrator-dialed channel (section 1.9).

### 1.3 What the guest sees

- Device: virtio-blk, `VIRTIO_BLK_F_MQ` with `num_queues = max_cpus` clamped to 32, one worker
  thread per enabled queue; `F_FLUSH`, `F_DISCARD`, `F_WRITE_ZEROES`, `F_TOPOLOGY`
  (`opt_io_size` = 1 MiB, `min_io` = one 4 KiB block, physical == logical); `F_BLK_SIZE` 4096;
  online grow with a real `config_generation`; shrink refused. Refused features: `CONFIG_WCE`,
  `GEOMETRY`, `BARRIER`, `SCSI`, `SECURE_ERASE`, `ZONED` (WS-VMM "Topology, and the features we
  refuse"). No FUA: durability is expressed only through FLUSH (spec G1 row).
- `discard_sector_alignment` = 4 KiB, derived from the backend geometry; misaligned trim segments
  are counted and passed through, never refused, because an `S_IOERR` on a trim would shut the
  guest filesystem down (WS-VMM "Discard and write-zeroes").
- Filesystem: XFS only, `crc=1`, `reflink=1`, `agcount=16` (a concurrency class, not per-vCPU;
  `mkfs.xfs -d concurrency=` refuses AGs under 4 GB), 4096 block size, mount
  `noatime,lazytime`, no inline discard, weekly `fstrim` timer plus `fstrim` after
  `nix-collect-garbage` (`block-volumes/WS-IMAGE-REPORT.md` "One image, XFS").
- Writeback shaping pinned in absolute bytes, not ratios: `vm.dirty_background_bytes = 64 MiB`,
  `vm.dirty_bytes = 256 MiB`, so the burst the host must absorb does not scale with guest RAM.
  Uncalibrated: "the throughput these are calibrated against is NEEDS MEASUREMENT" (WS-IMAGE
  "The writeback numbers").
- Timeout policy: virtio-blk installs no `.timeout` handler and generic blk-mq re-arms the
  deadline, so a stalled backend leaves the guest in D-state, patiently, not erroring
  (`WS-LOG-REPORT.md` FINDING 3; `crates/storage/blockvol/design/03`). Image pins
  `kernel.hung_task_timeout_secs = 300`, `hung_task_panic = 0`, `sysrq = 1`;
  `/sys/block/<dev>/device/timeout` does not exist for virtio-blk (WS-IMAGE "design/03's
  image-side obligations"). The minutes-scale stall policy (keep hanging vs pause the VM) is
  still an open product decision (WS-VMM DECISIONS-NEEDED 4).
- I/O scheduler `none`, set by a oneshot unit because udev is disabled in the guest and a udev
  rule would have been silently inert (WS-IMAGE FINDING 3).
- Boot chain `root=/dev/vda rootfstype=xfs`; no partition table, so LBA 0 is the filesystem's
  first block (WS-IMAGE "QUEUED FOLLOW-UP").
- df is real, ENOSPC is a normal guest event, thin provisioning is a host concern (spec section 6).
- Snapshot consistency contract: crash-consistent at an LSN cut, "exactly as if power was
  pulled". App-consistent freeze is out of scope because product snapshots include RAM (spec
  section 6).

### 1.4 The intent log (write path and durability)

- Per-volume, preallocated 256 MiB segment files with a 4096-byte header, 64-byte record header
  with crc32c, physical redo records keyed `(lba, len)`. Group commit: flush immediately when
  waiters exist, else every 50 ms. Replay = CRC-chained LSN prefix; torn tail on a `fallocate`d
  segment exits through the CRC into preallocated zeros, not through truncation (WS-LOG "Review
  pass"; `research/vcfs-respine/W9-intent-log.md`; inventory section 3).
- Log budgets as of 2026-07-28: window 6 GiB, ceiling 12 GiB, governor drain rate 220 MiB/s,
  idle checkpoint after 5 s / promote after 15 s, max pacing delay 500 ms, ceiling park timeout
  5 s, watermark wait timeout 60 s, 5 max consecutive flush failures (inventory section 3).
  The 12 GiB ceiling as a guest-parking mechanism was deleted on 2026-07-28 (section 3).
- The FLUSH obligation: a FLUSH must cover every write the driver has been told completed, on
  any queue. Mechanism: one per-volume `AtomicU64::fetch_max` folded in on the completion path
  before the used-ring push; FLUSH reads it once and waits the group commit to that LSN.
  "A barrier raised at submission is merely wasteful; one raised after the request became
  visible to the driver is a silent durability bug" (WS-LOG section 1). Tested with a simulated
  device across 1-16 queues and a negative control that proves the test can see the
  reordering (WS-LOG section 4).
- The index assigns the LSN inside the same critical section as the append, so log order equals
  LSN order and a gather at the high-water mark is consistent by construction. The S1 skeleton
  had them as separate steps, which let two writers append out of LSN order and invert
  last-write-wins on replay (`WS-ENGINE-REPORT.md` FINDING 1).
- Write-through fallback: a driver that negotiates neither `F_FLUSH` nor `F_CONFIG_WCE` must
  get every mutation settled before completion. The S1 device kept a private atomic so this
  could never engage; silent data loss for such a guest (WS-LOG FINDING 1).
- A poisoned log (an append failed and dropped the record) must fail the barrier with IOERR
  rather than ack a watermark it cannot publish; the honest fix is a demand fold to CAS plus
  image `fdatasync` as the barrier (WS-LOG FINDING 2, DECISION-NEEDED 2).
- An empty discard seeded its ack from the next unassigned LSN and wedged the next FLUSH forever
  (guest in D-state, "reads as a storage bug"). Fix: an empty trim acks LSN 0 (WS-LOG FINDING 3).
- The guest write never waits on CAS, network, or fold. Backpressure is latency, never
  `BlockError`, because IOERR means XFS shutdown and a dead VM (inventory invariants).

### 1.5 The RAM index, fold, and extent map

- RAM extent index: latest-per-block at 4 KiB granularity with a per-block sector mask; block
  bytes are `Arc<[u8]>`. Sub-block writes defer read-modify-write to the fold because resolving
  the uncovered sectors means reading the committed map, which means CAS, which a write may not
  wait on (WS-ENGINE "Design decisions").
- Fold cadence: governor tick 250 ms, dirty-entry forcing cap 32,768 (chosen from the freeze
  curve, see section 2), settle 1 s, one fold in flight, tombstone compaction at 4096, read-fold
  8 attempts / 5 s permit wait (inventory section 3). Tempo rule: a fold never holds anything a
  FLUSH needs, asserted at runtime (WS-ENGINE Gate 5).
- Fold work: gather latest-per-block, coalesce into contiguous runs in LBA order, detect zero
  runs, CDC chunk with boundaries snapped down to the 4 KiB block (an extent names exactly one
  chunk, so a straddling boundary would be silent corruption), `put_chunk_batch`, build extent
  ops, commit the tree, write the manifest blob `{shape, tree_root, cut}`, publish head, then
  retire log entries ("head published before entries retired, inside the fold": reversed order
  leaves an instant where bytes are in neither place) (WS-ENGINE FINDING 2; inventory
  invariants).
- Chunker: CDC 16/64/256 KiB (min/avg/max) (`crates/storage/blockvol/src/fold/chunking.rs:19`,
  `snapshot-design/00-measurements-brownout-run.md`).
- Extent map (`crates/storage/blockvol/src/extent.rs`, `block-volumes/WS-MAP-REPORT.md`):
  prolly tree from the vcfs `volume-index` crate instantiated with big-endian LBA keys under tag
  byte `0x10`; values `Chunk{hash, offset_in_chunk}` or a zero marker; 4 KiB granularity;
  chunk-backed extents capped at 1 MiB (dead weight at 256 KiB chunks); zero extents unbounded;
  split on partial overwrite, merge only in fold. Nodes are content-addressed and live in CAS,
  so the map is self-hosting; snapshot = root hash at an LSN cut; branch = new head, same root;
  diff is O(delta) node visits. The extent's chunk hash must be registered in
  `Page::direct_child_hashes` because that set is the GC refcount edge set: an unregistered
  hash means GC frees a chunk a live snapshot still reads (WS-MAP "The value axis").
- Discard punches holes (tombstones keyed by extent start); `write_zeroes_unmap` is literally
  `discard` at the map layer because an absent range reads as zeros; unaligned discards round
  inward, unaligned writes are refused (WS-MAP FINDINGS 4, 5). A fold's discard path must READ
  the committed extents covering the range before it can emit tombstones, so discard cost
  scales with extents covered, not bytes (WS-ENGINE IN-PROGRESS 2; design/08 title).
- Device capacity cannot live in the tree (keyed by LBA, no scalar slot); the manifest carries
  it beside the root, or a shrink-then-grow hands the guest bytes from a previous life (WS-MAP
  FINDING 6, 7).
- Every write-amplification number in the engine report is the CAS leg only; add the log leg for
  total physical. A vcfs helper counting only the CAS leg once reported 0.10x for a volume whose
  total was 1.64x (WS-ENGINE "Every number here is the CAS leg").

### 1.6 CAS, placement, replication, and the head registry

- CAS: blake3-addressed chunks in a per-node LMDB catalog opened `WRITEMAP | MDB_NOSYNC`; a put
  records local presence and outbound replica intents `(hash, peer)` in one LMDB transaction;
  a drain task pages 256 intents at a time, groups by peer, probe -> promise -> local get ->
  `BatchPutKeyed` over QUIC (6 independent lanes, each its own endpoint / UDP source port),
  paced at 128 MiB/s; 32 concurrent pushes; peer timeout 10 s (inventory sections 3, 4;
  `snapshot-design/11-replication-drain-classes.md` RC3, RC4).
- Placement: straw2 rendezvous hash, `replica_count = 2` default
  (`crates/storage/cas/fabric/leader/src/config.rs:211`). A chunk therefore has one remote owner
  to satisfy; chunks are spread across all five peers, not mirrored (drain-classes 1.4).
- The peer's put ack is "received/indexed", not stable media. `StableFence` exists (dirty
  segments `fdatasync`ed, then catalog synced, single-flight rounds) and is what the watermark
  design wires in (`11-watermark-contract.md` section 5).
- Head registry: Postgres `blockvol_volume_heads` per (volume, branch): `live_root_id`,
  `live_lsn`, `durable_root_id`, `durable_lsn`, `replicated_lsn`, `base_root_id`,
  `writer_epoch`, `writer_node_id`, `writer_incarnation`, and a single-use handoff triple.
  Regional table, written only by the node-agent, every write fenced on `writer_epoch`. Artifact
  rows (`snapshot-<uid>`, `golden-<uid>`) share the table at epoch 0 (inventory section 1;
  watermark contract section 1).
- Publisher loop, every 5 s: (1) `DrainVolume` (fold through the live watermark, idempotent at a
  cut), (2) fenced `publish_live_head` under the VM operation lock, (3) durable confirm, (4)
  `promote_durable_head`, (5) `AdvanceCheckpoint` clamped to a published manifest so the log
  never unlinks a segment no manifest names, (6) release parked cut. Ticks are strictly
  sequential; a tick must skip, not queue, when the lock is held (a stale-head bug #8564)
  (inventory sections 1, 2).
- Boot resolution: same-node opens Live (local log high-water covers the row), cross-node opens
  Durable only when `durable_lsn > 0`, ambiguity refuses; a failed Postgres read fails the start
  rather than silently opening at base. Local state is judged by evidence (high water vs
  `durable_lsn`), not by `writer_node_id`; identity refusal once "bricked healthy returns"
  (inventory invariants; `crates/.../head/boot.rs`).

### 1.7 Base images, first boot, templates, forks

- Base image: `blockvolBaseArtifact` = one flat blob set (data chunks, extent-tree pages,
  manifest blob, all named by blake3 of their bytes) plus a `root` file, produced at nix build
  time by chunking only the allocated extents of the sparse XFS image (`SEEK_DATA`/`SEEK_HOLE`;
  65 MiB actual against 512 MiB apparent on a test image). This replaced a raw image file
  because the movement engine zero-fills every range the extent map does not cover and the
  manifest carries no base reference, so a volume seeded from a raw file drained a manifest that
  did not describe its own disk: "a silently corrupt root filesystem produced by the snapshot
  path" (WS-IMAGE "QUEUED FOLLOW-UP"; design/06).
- First boot: seed the artifact into local CAS (6.16 GB / 89,118 blobs / 24.2 s at 254 MB/s
  from a local nix-store path), which emitted 89k outbound replica intents per node until the
  seed-exemption stamp (image chunks are region-durable by the deploy push, so a stamped node
  runs no seed). Every VM branches from golden images, so identical blocks are the same chunks
  until diverged (`snapshot-design/04-m1-results.md`; `12-decisive-run.md` seed row; spec
  section 8).
- Image build churn measured: about 16 image-invalidating commits a day (115 in 7 days), and
  nothing reaped superseded blob sets while GC was gated (WS-IMAGE "Measured: how often the
  block image actually changes").
- Templates (`templates-design/00-design.md`, `01-images-are-templates.md`): a template is any
  flake that evaluates to a NixOS system; first `ix new` builds it in-guest, assembles the
  closure into a CAS closure manifest, caches it by (pinned rev, attr, profile hash). Images
  are "compiled templates"; user-facing image push was deleted after measuring zero customer
  pushes ever. Snapshots are captured state; templates are compiled source. Non-goal: "No
  change to snapshots, blockvol, or the boot path."
- Fork/clone is NOT a verb in the product (user decision 2026-07-28); callers of the capture
  engine are snapshot, golden, migration (`snapshot-design/10-design-v0.md` section 10.4).

### 1.8 Snapshots

Capture path after the 2026-07-28 redesign (`10-design-v0.md`, `11-watermark-contract.md`
section 4.5, `02-current-system-inventory.md` invariants):

1. Pause vCPUs; `pause_blk_queues` (fence until no worker can write guest memory, 5 s
   `QUIESCE_TIMEOUT`, with the pacing governor and ceiling park bypassed inside the fence).
2. `export_cut` in-process: freeze the RAM index at the FLUSH completion watermark (clone the
   dirty map, one refcount bump per block, O(dirty entries) B-tree allocation), park it, pin the
   base tree. The cut IS the FLUSH watermark, taken after blk quiesce and BEFORE the RAM capture:
   "disk ahead of RAM = journal replay; disk behind RAM = corruption".
3. RAM capture (write-protect arming for alive captures, `CaptureDead` terminal for migration).
4. Resume. The RPC returns at `captured` = cut LSN + ownership + artifact row + parked view.
   Nothing else is on the RPC path.
5. Async fold of the parked cut (`drain_exported`) produces the manifest at exactly
   `through_lsn == cut_lsn`, fills root + seal on the artifact row (`pinned` -> `folded`),
   releases the parked view.
6. `ready` when the volume's `replicated_lsn >= cut_lsn` and the seal (chunk count, byte count,
   blake3 over digest-ascending chunk digests) verifies against what peers fenced to disk.
   Ready is peer-disk (DRBD protocol C), because `ready` authorizes deleting the source.

Restore = seed a head row from the artifact, open the root, fault chunks lazily. Same-node
restore needs `folded`; cross-node needs `ready`. A failed capture must leave the next delta a
strict superset (QEMU bitmap retention rule), which the parked-cut release gives.

### 1.9 Migration: what moves, how the cut is taken, what it costs

RAM: QUIC precopy rounds up to `precopy_max_iterations = 5`, then postcopy demand faults through
`RemotePageSource` with `per_fetch_deadline_secs = 30`, plus a residual push on a second bidi
stream (#10119). RAM transport was fully implemented before the disk half existed
(`snapshot-design/03-migration-substrate-gaps.md`).

Disk (`migration-design/01-node-cutover-contract.md`, `02-disk-cut-dark-window.md`,
`snapshot-design/10-design-v0.md` section 7):

- During precopy the ordinary publisher ticks (5 s) keep folding, so the replication frontier
  tracks the append point. The 2026-08-07 measurement showed every terminal fold was a replay:
  `rounds=1 tail_bytes=0`, fold 3-5 ms.
- Terminal cut inside the same vCPU-paused window as the RAM freeze: atomic LSN grab + parked
  frozen view, 3-6 ms ("the only irreducible in-window disk work").
- `terminal_publish_and_handoff`: final `(live_root_id, live_lsn)` AND the single-use handoff
  marker (successor node, incarnation, epoch) in ONE fenced UPDATE `WHERE writer_epoch = $e`.
  No window where the final pair exists without the marker.
- What crosses the wire for disk: the `CutoverOffer` names `(migration_uid, cut_lsn, cut_root,
  authorization_incarnation)` plus device state and the residual RAM manifest. The offset-to-
  hash map itself is already in CAS (the manifest root); the target opens it and faults chunks
  from CAS, which the still-alive source serves.
- Claim: `acquire_volume_writer_as_successor`, a conditional UPDATE that tests and clears the
  marker in one statement (`handoff_epoch = writer_epoch`), so it succeeds at most once ever.
  Revoke is the mirror conditional UPDATE keyed on the same epoch. "Postgres decides. No
  consensus, no quorum, no lease clock, no leader arbitration." The outcome column is written in
  the same transaction, so the stale sweep reads the migration row alone and gets a total
  function (C-COMMIT-ONE-WRITER, C-DECIDE-ATOMIC, C-SWEEP-DECIDES).
- Source teardown is gated on `await_watermark(cut_lsn)`: replication gates the teardown, never
  the cut ("It waits for the FOLD, never for replication").
- Failure policy: source dies after seal with a non-empty residual -> target refuses and the
  row revokes (guest survives on the source); target dies before claim -> source revokes and
  unpauses in under a second (was 600 s); partition -> both attempt conditional writes, one
  wins. A node that cannot reach Postgres never runs the guest (C-OWN-BY-WRITE).
- Adoption after a node-agent restart mid-protocol asks "is there an unspent marker naming me",
  not "what phase is the row in" (C-ADOPT-WINDOW). Boot resolution must never read the handoff
  columns: a marker authorizes an acquire, it is not evidence about which head is safe.

Costs, all measured on prod hil nodes with an idle guest:

| Date | Blackout | Bytes moving | Dominant term |
|---|---|---|---|
| 2026-07-30 | 8-10 s | < 9 MB | eight leader-brokered phase transitions, 10 s tick rounding (2.4 s and 3.2 s dead gaps) |
| 2026-08-07 (A-E stack, 6 legs) | 2.89-3.02 s | < 7 MiB | `DrainingSource` 0.32-1.49 s per leg, of which the disk cut is 3-6 ms; 96-99% is getting a handler to run |
| 2026-08-07 wedge | 607 s dark | n/a | target stood up its QUIC slot and never restored; nothing shorter than the 600 s stale sweep existed |

`01-node-cutover-contract.md` section 1 and `02-disk-cut-dark-window.md` section 2. "The window
is orchestration-latency-bound, not bandwidth-bound."

---

## 2. Every measured number, target, budget, and failure

### 2.1 Host hardware and raw device floors

| What | Value | Source |
|---|---|---|
| Nodes rented | OVH Scale-a5 (EPYC 9554/9555, 64c/128t) and Scale-a8 (EPYC Turin 9965, 192c/384t); 128 GB DDR5 4800 ECC; 2x 1.92 TB NVMe RAID; 5 Gbps public; 50 Gbps vRack private, no upgrade option; $638 / $816 / $1,147 per month | `hardware/ovh/pricing.md` |
| Dev/hil NICs | 4x25GbE, 2x25 bonds (`bond-vrack` 50,000 Mb/s); dev = Intel i40e, hil = Mellanox mlx5_core | `10-design-v0.md` section 5 |
| Multiplexed QUIC ix-rpc sample | 25-27 Gbit; 8 endpoints 27.6-29.6 Gbps vs 9-10 Gbps for one | `10-design-v0.md` 0.5; `11-replication-drain-classes.md` RC3 |
| Host NVMe (Samsung MZWL6 md RAID1) O_DIRECT 1 MiB seq append | 1768 MiB/s, clat avg 561 us; with fdatasync per 64 KiB 1283 MiB/s | `research/vcfs-respine/W9-intent-log.md` |
| Log append media ceiling | 2.1 GiB/s | spec section 7 |
| fdatasync latency | avg 35 us, p99 78 us, p99.9 758 us, p99.95 3.5 ms, max 19 ms (PLP write cache) | W9 |
| ext4/mdraid append+fdatasync p50/p99 | 4 KiB 0.060/0.162 ms; 64 KiB 0.085/0.180; 1 MiB 0.381/0.506; 8 MiB 1.908/2.481 | `research/vcfs-respine/BARRIER-FLOOR.md` |
| ZFS+SLOG same | 4 KiB 0.635/0.944 ms; 64 KiB 0.331/1.437 (max 15.2); 8 MiB 6.134/13.091 (max 28.1; busy max 64.0) | BARRIER-FLOOR |
| Fabric RTT (mTLS QUIC `Have`) p50/p99 | local 0.063/0.242 ms; peer on vRack 0.108/0.257 (max 8.0); storage node 0.799/4.025; busy local max 79.2 ms | BARRIER-FLOOR |
| Postgres 18.4 `synchronous_commit=on` | p50 0.175 / p99 0.462 ms | BARRIER-FLOOR |
| Replicated barrier floor (K=3 owners, parallel) | p99 3.129 ms idle / 2.333 busy; 64 KiB component floor 5.924 ms; the slowest owner (ZFS/SLOG) set the tail 10/10 samples at 95.7% of barrier time; proposed fsync SLO 10 ms p99 | BARRIER-FLOOR |
| Raw durable 4 KiB LMDB commit | 16.5 us | `snapshot-design/07-u4-measurement.md` (superseded note) |
| Channel handoff (old vcfs shared-memory ring) | 5-25 us; mapping_open residency 4.3 us | `research/vcfs-respine/README.md`; spec section 7 |

### 2.2 Log, fold, map (T1 in-process benches, dev-compute-6, real fdatasync, real CAS store)

| What | Value | Source |
|---|---|---|
| Overwrite storm: 32 MiB rewritten 16x in 64 KiB writes, fdatasync per pass, 512 MiB guest bytes | fold every 1/4/16 passes -> CAS data 247 / 61.8 / 15.4 MiB; uncoalesced ratio 1x / 4x / 16x; WA (CAS leg) 0.482 at every cadence (synthetic repetitive content halves it; real data "closer to 1x"); map nodes 1.03% of coalesced bytes; max fold 45 ms; zero tempo violations | `WS-ENGINE-REPORT.md` Gate 2 |
| Freeze (`export_cut`) inside the pause | 10 MiB / 2,560 blocks: 261 us (102 ns/block); 100 MiB: 4.28 ms (167 ns); 1 GiB / 262,144 blocks: 79.5 ms (303 ns/block), superlinear | Gate 2b |
| Dirty-entry cap chosen from the curve (worst of 7) | 16,384 entries 2.56 ms; 32,768 5.29 ms; 65,536 13.30 ms (cap, inside 25 ms engine share); 98,304 41.70 ms | Gate 2c (inventory later lists the forcing cap as 32,768) |
| Drain latency vs delta | 10 MiB 12.1 ms; 100 MiB 160.2 ms; 1 GiB 1.5 s; 1.21-1.60 ms/MiB flat | Gate 3 |
| Drain latency vs volume size | 10 MiB delta: 12.0 ms empty vs 12.6 ms with 1 GiB committed (5%) | Gate 4 |
| fsync under fold with CAS slowed to 1 s/put | every flush < 100 ms across 6 folds | Gate 5 |
| Extent-map property test | 100,000 ops, 2,949 snapshots retained, 4.2 s; caught a data-resurrection bug on granule 350 at first run | `WS-MAP-REPORT.md` |
| Diff cost | 10 page fetches for 1 changed extent in a 2,000-extent map (64 pages/tree) | WS-MAP |
| Map bytes per entry (compressed pages) | sequential 256 KiB chunks 40.3 B/entry (page overhead); random 4 KiB 24.9; 16 KiB-stride 8.8; LBA-ordered packing worth about 3x; degenerate hashes flatter by 1.6x | WS-MAP "Map size" |
| Map size extrapolated to a full 100 GiB volume | sequential ~17 MB; random 4 KiB ~653 MB; random folded in LBA order ~231 MB; fixed 64 KiB blocks (option B) ~41 MB | WS-MAP D2 |
| Log conformance | 67 + 19 tests; hegel properties 24 and 48 drawn shapes | WS-LOG |
| Test flake | `a_flushed_write_survives_a_reopen_with_no_fold` ~40% flaky (fold ran before the assertion) | WS-MAP FINDING 8 |
| Counter file coalescing | flush kicks a publisher; floor 25 ms -> 1 s; heartbeat 5 s | WS-LOG FINDING 4 |

### 2.3 Live snapshot, replication, and convergence (dev-compute-3, 2026-07-28, M1-M3 and gates)

| What | Value | Source |
|---|---|---|
| Idle snapshot, warm | total 3.57 s / 1.11 s; manifest 2.6 s / 0.69 s; guest pause 238.6 / 40.4 ms; CLI wall 3.61 / 3.15 s (RPC overhead ~2 s dominates) | `04-m1-results.md` |
| 1 GiB `dd oflag=direct` in guest | 434 MB/s; snapshot total 36.7 s = publish_cut 35.1 s + manifest 1.1 s; pause 82 ms; storage_cut 1 ms; guest-data durable rate 29.2 MiB/s end to end | M1 |
| Replica-intent drain ceiling | ~800-830 intents/s = 48-51 MB/s, 0.86% of the 50 Gb/s bond; per-peer put_keyed mean 152-191 ms (~19 MiB/s per stream, x5 would be ~95); the 99%-full peer disk was the fastest; reconfirmed three times (M1, M2 ~1,339-1,637/s, M3 ~860-1,151/s) | M1, `05-m2-results.md`, `06-m3-results.md` |
| Bytes per intent | 60.5 KB bulk (64 KiB CDC avg); 17.3 KB under 8 KiB-page OLTP (~3.5x more intents per MB) | M2 |
| Write amplification on the wire at RF~2 | 1.76x (M1), 2.03x (M3), 2.0000x (U1) | M1, M3, `08-u1-gate.md` |
| Reference OLTP (8-client pgbench, 610 TPS) | 1,729 intents/s produced vs 1,339/s drained; backlog monotone to 708,984; 12 GiB log ceiling at 835 s; guest writes parked 7 m 40 s, zero completed transactions for 130+ s; latency stddev 698 ms | M2 |
| Forced ceiling (16 GiB dd) | checkpoint pinned 176 s so debt = cumulative writes; dd 441 -> 24 MB/s over 178 s (18x pacing decay), then 0.1 MB/s for ~135 s (35 parks), instant recovery to 311 MB/s; post-dd drain 121 s | M3 |
| Bulk convergence | unthrottled deficit ~380 MB/s; +16.4 MB/s deficit even with the guest throttled 6x | M3 |
| Snapshot outcome depends on workload shape | bulk mid-write: all 6 captures ready honestly (publish_cut 28-55 s); OLTP mid-load: snap1 died after 350 s publish deadline + 600 s confirm never passing batch 1 of 60; snap2 ready at 696 s while the CLI reported failure at its 180 s deadline; snap3 post-load 78.3 s because the capture itself dirtied 551k RAM pages | M2, M3 |
| Pause starvation mechanism | `pause_blk_queues` 4.77 s (brownout run) and 3.415 s (M3) = capture quiesce waiting out the remainder of a 5 s ceiling park; `DEFAULT_CEILING_PARK_TIMEOUT == QUIESCE_TIMEOUT == 5 s` exactly; arithmetic closes to 2 ms; 0-1 ms in every other regime | `00-measurements-brownout-run.md`, M3 |
| Pause outside the ceiling regime | 70-250 ms, dominated by WP arming (42-97 ms bulk; 430-510 ms at 1.1 M dirty OLTP pages); first capture on a fresh VM ~2.5x later ones | M2, M3 |
| Post-capture disturbance | OLTP TPS dip 10-12% for 10-20 s at capture, plus a second dip to 133 TPS ~40 s later (WP fault handling); absent under bulk; gone after the redesign | M2, M3, `09-gate-a.md`, `12-decisive-run.md` |
| Brownout run (first boot, unstamped) | debt 12.94 GB > 12 GiB ceiling for the whole run; backlog 148-171k intents; net drain 156/s while writing, ~1,114/s pure; ready-flip confirms 36.7 s idle, 290-310 s busy; inline confirm failed 4/4 | `00-measurements-brownout-run.md` |
| Per-intent delete cost | 560 us/delete, strictly serial: 500 us `MAX_COMMIT_LINGER` + 60 us commit; 97.2-97.3% of committer batches on prod waited the full window to fold in nothing (mean fill 1.03 ops) | `07-u4-measurement.md` |
| Seed | 6.16 GB / 89,118 blobs into local CAS in 24.2 s (254 MB/s); 89k outbound intents; peak backlog 25,674; first VM boot failed (remote-bootstrap 8 s budget) then paid a 64 s golden capture; stamp costs 2.8-3.3 s / 1 blob / 64-73 KB; stamped cold node boots in 4.0 s with no seed | M1, `09-gate-a.md`, `12-decisive-run.md` |
| Kick churn during seed | 136 drain passes cancelled by kicks, 15 budget exhaustions; `await_replicated` waits of 2,886 / 6,376 / 13,768 / 14,631 ms, all `drained` (pure queueing) | drain-classes 1.5, 1.6 |
| After redesign (decisive run, dc3+dc5) | checkpoint 46 distinct advancing values at 88% of write rate; debt sawtooth peak 3.41 GiB; mid-load snapshot ready in 152.9 s; pgbench min 697.8 / peak 3,356 TPS at 2.4 ms; invariant zero held through an 18x-over-budget peers-down outage with 0 stalls | `12-decisive-run.md` |
| U1 gate (governor bypass in the fence) | ceiling-parked capture: `pause_blk_queues` 0 ms, pause 130 ms (was 3,415 / 3,512 ms) | `08-u1-gate.md` |
| Cross-node restore (pre-existing main defect) | worker blocks pre-accept on a 120 s full-RAM preload (54,358 sequential cross-node gets) against a 30 s orchestrator deadline; same-node 8.2 s; "sixth instance of the readiness-seam class" | `12-decisive-run.md` leg e |
| Storage fill incidents | dc1 at 1.8 GiB free of 1.7 TB; CAS GC disabled fleet-wide; `ix rm` frees nothing; three fill incidents in one day | `12-decisive-run.md` |

### 2.4 Migration (prod hil, idle guest)

| What | Value | Source |
|---|---|---|
| Blackout, leader-phased | 8-10 s with < 9 MB moving; dead gaps 2.4 s and 3.2 s from 10 s tick rounding | contract section 1 |
| Blackout, A-E stack, 6 legs | 2.89-3.02 s, < 7 MiB moving | `02-disk-cut-dark-window.md` |
| `DrainingSource` per leg | 324 / 906 / 1,048 / 1,492 ms total; phase write -> decision 267 / 848 / 991 / 1,432 ms; terminal cut 3-6 ms; record -> leader advance 53-54 ms (50 ms debounce + ~3 ms NOTIFY) | `02-disk-cut-dark-window.md` section 2 |
| Terminal fold | `fold_ms` 3-5 on all seven captures; `rounds=1 tail_bytes=0` on every leg | same |
| Wedge | 607 s dark; target never restored, empty `failure_reason`; 600 s stale sweep was the only deadline | contract 3.2.1 |
| Leader freeze incidents | 2026-07-29 task-loop freeze parked a customer migration at `capturing_source` for 80+ minutes with the guest frozen; version-skew zombie leader did zero reconciliation; 2.6/s retry storm from `set_failure_reason` never moving `updated_at` | contract 2.1, 5.4 |
| Budgets | `cutover_timeout_secs` 60; `stale_timeout_secs` 600; `per_fetch_deadline_secs` 30; `APPLY_IDLE_TIMEOUT` 5 min; `TARGET_APPLY_WAIT_DEADLINE` 2 min; `DEFAULT_PAUSE_BUDGET` 300 ms (QEMU `downtime-limit`, later deleted); `precopy_max_iterations` 5; migration wake debounce 50 ms; poll fallback 10 s; in-window step deadlines deliberately unset pending a two-node p99 measurement | contract 3.3, 10, 11 |
| Headline win of the direct protocol | target-dies-before-claim: source guest running again in under a second vs 600 s | `PR-D1-BODY.md` kill-leg 6 |
| Confirm cost the seal avoids | CAS tree diff dominated the honest barrier at ~16 ms p50 (#7769) | `WS-ORCH-REPORT.md` Gap 2 |

### 2.5 Budgets and constants on the volume path (2026-07-28 inventory, section 3)

Tick 5 s; backoff max 60 s; `DRAIN_TIMEOUT` 120 s; `ARTIFACT_DURABLE_BUDGET` 600 s;
`FINAL_PUBLISH_BUDGET` 45 s; capture-unwind release 30 s; `CONFIRM_BATCH_MAX_HASHES` 4096;
memo cap 1,048,576 hashes; `AWAIT_REPLICATED_DEADLINE` 30 s + 5 s/window (a 4096 batch = 350 s);
64 serial await windows; LAPIC ISR drain 50 ms; chunk cache 256 MiB; materialize warm 3 levels /
256 pages; discard-filter budget 1 ms; segment 256 MiB; replica drain interval 1 s, pacer
128 MiB/s, 32 pushes, page 256, peer timeout 10 s, kick budget 8/pass, 4096 queued kicks;
batch-put wire caps 8 MiB / 65,535 chunks; extent granularity 4096 B, max extent 1 MiB.

### 2.6 Boot and build timings

| What | Value | Source |
|---|---|---|
| Warm boot | 850 ms (M2); 4.0 s on a stamped cold node (decisive run) | M2, `12-decisive-run.md` |
| Blockvol boot tax | every blockvol boot also launched a vmfsd and materialized a vcfs rootfs it never opened (magnitude unmeasured) | `WS-ORCH-REPORT.md` FINDING A |
| Image packing VM | 0.75 s boot, 1.5 s format-mount-copy-verify | WS-IMAGE FINDING 2 |
| Cold nix closure builds | 3 h 05 m killed with nothing cacheable produced; 3,920 derivations; GHC and two LLVMs from source; load 140-264 on 32 cores | WS-IMAGE, WS-VMM, WS-ORCH |

### 2.7 Failures the reports recorded that were not numbers

- `ix snapshot create` on a block-volume VM succeeded with the wrong disk: it cut the pristine
  vcfs rootfs the VM never wrote to, so restore would have silently lost every guest write
  (WS-ORCH "Items 2 and 3").
- The oracle `doctor` printed `RESULT PASS` unconditionally (subshell lost the flag); a SKIPPED
  fault leg printed `VERDICT: PASS` (WS-LOG "Correction").
- The image drift gate compared config against files generated from the same evaluation and
  could not fail; the earlier XFS path would have shipped an empty filesystem; `xfs_protofile`
  truncates on a filename with a space and `mkfs.xfs` reports success (WS-IMAGE FINDINGS 1, 2,
  11).
- `list_active_for_node`'s hand-written SELECT omitted three new columns; SeaORM mapped them to
  `None` silently, so every decision read as undecided (`PR-D1-BODY.md` "Validation status").
- `vm.dirty_ratio` set by any module sorts after `vm.dirty_bytes` in `60-nixos.conf` and
  silently zeroes the writeback shaping (WS-IMAGE FINDING 11).
- Mutually silent auto-merge duplicated identical functions across two branches, 7 compile
  errors with no git conflict (`07-u4-measurement.md` merge notes).
- Intent tracker counted intended stages before the write; LMDB put overwrote on duplicate key,
  orphaning a manifest count forever and pinning the checkpoint (gate A root cause).
- `promote_durable_head` passed `durable_lsn` where the replicated watermark was specified, so
  under sustained writes the checkpoint never moved (re-gate round 2).

---

## 3. Decisions reversed or regretted, and why

Storage architecture:

1. **File-level CoW filesystem (vcfs) -> block volume.** Nine waves of respine work (W1-W9,
   `research/vcfs-respine/`) improved lookups 21x and writes 76% and still left `nix build`
   failing on POSIX corners. The pivot doc calls the file/block split the wrong place for the
   moat (spec section 1). `research/vcfs-respine/ENDSTATE.md` had excluded "No node-local WAL",
   "No replicated syscall log", "No block device under a local fs"; all three were reversed.
2. **No node-local WAL -> intent log (W9).** The FUSE-era WAL was deleted in #5814; W9 reinstated
   it because guest fsync p99.95 was 9.6 s (max 13.4 s) with CAS draining at 13.4 MiB/s while the
   local NVMe took an fdatasync in 35 us (`W9-intent-log.md`).
3. **Durability tiers (`BarrierRelease::{Admitted, Durable}`, demand-commit bell,
   `FsyncPolicy::LocalAck` acking RAM in ~88.7 us) -> one release point, `Logged`.** "Applications
   destroy their own recovery fallbacks" (`W6-durability-barrier.md`, `WS-CORDON-REPORT.md`
   section 5 shows three docs claiming stronger durability than the code delivered).
4. **`AwaitDurable` on the CAS meant page cache** (`MDB_NOSYNC | WRITE_MAP`, store opened
   `durable:false`); renamed and later fenced with `StableFence` (`W6.1-STABLE-TIER.md`,
   watermark contract section 5.1).
5. **Per-VM daemon (`vmfsd`) -> backend in-process in the VMM.** Deleted in #8750, accepted
   cost: crash-consistent, not restart-transparent (`volume-backends.md`; spec G3).
6. **12 GiB debt ceiling that parks guest writes -> deleted ("invariant zero").** M2 froze a
   610-TPS guest for 7 m 40 s at 0.37% link utilization; M3 showed the park timeout equals the
   capture quiesce timeout exactly (`10-design-v0.md` 0.5, section 3).
7. **Per-hash durable confirm (4096-hash batches, 64-hash serial windows, 1 M-hash memos,
   30 s + 5 s/window deadlines, 600 s retry, tree-diff delta re-derivation) -> per-manifest
   replication watermark + per-artifact seal.** Three independent deadlines (180 s CLI / 350 s
   publish / 600 s confirm) disagreed about one capture's fate (`10-design-v0.md` sections 2,
   4, 9; `11-watermark-contract.md`).
8. **Inline confirm and inline fold on the capture RPC -> metadata-only capture.** publish_cut
   was 92-96% of capture wall and spanned three orders of magnitude (16 ms to 55 s) on the same
   VM (`10-design-v0.md` section 4).
9. **Kick / cancel / rewind urgency in the drain -> declared classes + in-memory interest
   registration, nothing cancelled.** Un-budgeted kicks livelocked a node for 12 hours
   (ix#8330); budgeted to 8 they stopped working after 8 waits (`11-replication-drain-classes.md`
   RC2, 4.1).
10. **Two replication classes -> three tiers (Interested / Steady / Bulk).** The measured
    collision was seed-vs-steady, which two classes could not express (drain-classes 3.3, 9).
11. **"Eliminate the probe; an intent proves the peer lacks the chunk" -> false.** Intents exist
    for chunks peers already hold; the probe stays (drain-classes 4.3; design-v0 section 5
    CORRECTION).
12. **Seal digest "in LSN order" -> digest-ascending.** No global LSN order over chunks survives
    coalescing and dedup; the repair path yields a set (watermark contract 7.2).
13. **W-ADV-PUB "an unpublished manifest never blocks the watermark" -> "...once its rows have
    drained".** Measured 33,231 uncounted rows across 25 manifests in 5 minutes; the unqualified
    rule fail-opened in steady state (watermark contract ADDENDUM 3).
14. **Rolling-deploy safety via a one-time backfill and a `GREATEST` in one query -> schema
    trigger.** The invariant "lived in exactly one query old binaries do not run"; and the
    trigger now fabricates a watermark on every durable promote, a standing hazard (ADDENDUMs 1,
    4).
15. **`MAX_COMMIT_LINGER = 500 us` -> deleted.** 97.2% of prod committer batches waited the full
    window to fold in nothing (`07-u4-measurement.md` superseded note).
16. **Raw image file as the volume base -> pre-chunked blob artifact + manifest root.** Zero-fill
    of unmapped ranges made snapshots of image-seeded volumes silently corrupt (WS-IMAGE
    "QUEUED FOLLOW-UP", design/06).
17. **`drain_through_lsn` rounding the cut up -> exact frozen cut.** For an alive capture,
    rounding up puts post-resume writes into a manifest paired with earlier RAM (WS-ENGINE
    "Exact LSN cuts").
18. **Rewrite the counter file on every FLUSH -> coalesced to 1 s.** 40 fdatasync+rename pairs a
    second for a reader that looks once a second (WS-LOG FINDING 4).

Migration:

19. **Leader-brokered cutover (8 phase transitions, each a Postgres write + NOTIFY + tick) ->
    direct node-to-node protocol with exactly one Postgres round trip in the dark window.**
    Leader freezes had left guests frozen 80+ minutes (contract sections 1, 5.4).
20. **"Extend the existing target-dialed postcopy channel" (chosen in 3.1) -> superseded the same
    day.** The target end is `orch-vmm-worker`, which has no DB pool and must not get one; and
    routing cutover lanes inside the postcopy driver deadlocks because a paused guest faults on
    nothing so the receive loop never reads the socket (contract 3.1.1). `RAM_CHANNEL_METHOD_V2`
    and `supports_cutover()` were deleted as capabilities that lied.
21. **One-stream rule -> retired.** #10119 already ran a second bidi stream; version skew is
    handled by the open method string instead (contract 3.1.1).
22. **`DrainingSource` convergence loop (predict tail vs 300 ms pause budget, up to 8 rounds) ->
    deleted; disk cut moved into the capture's pause window.** The loop ran after the guest was
    already frozen, moved zero bytes, and its real exit was a replication-progress wait with the
    guest dark, exactly what invariant zero forbids (`02-disk-cut-dark-window.md`).
23. **VIP revert on stale `VmResumed` rows** pointed traffic at a frozen source while the guest
    ran on the target; fixed by routing on the decision, not the phase (`PR-D1-BODY.md` Defect A).
24. **`vip_settle_secs`, in-window route-cache drain timer** deleted: "a paused guest serves no
    connections" (contract section 9).

Process and scope:

25. **Per-VM `volume_type` column, mixed-node support, `--volume-type` flag, `BlockVolUnsupportedOnNode`**
    built and then deleted under the no-coexistence rule; the report keeps the reasoning
    (`WS-ORCH-REPORT.md` "RE-SCOPED").
26. **`IX_BLOCKVOL_ROOT_FSTYPE` seam** proposed, implemented, withdrawn: "a knob would model a
    choice the product deleted" (WS-ORCH FINDING E; WS-IMAGE FINDING 9).
27. **ext4 image variant and builder parameter** deleted rather than kept behind a knob nobody
    may turn (WS-IMAGE "One image, XFS").
28. **Prolly `V` genericization (charter) vs appending a leaf variant (G0 lock).** The lock won:
    a type parameter through 1,380 lines of page code has a silent-corruption mode if size hints
    and encoders drift (WS-MAP FINDING 1).
29. **Image push and the OCI registry surface** deleted after measuring zero customer pushes ever
    (`templates-design/01-images-are-templates.md`).
30. **User-facing image concept -> templates** ("a tag is a template row that has forgotten its
    source").
31. Research-era: T1 per-folio readahead flood executed and reverted (#7521); completion-batched
    namespace publication reversed after an A/B (#7688); host LockManager deleted with zero
    callers; batch-setattr opcode infeasible; fast-lane bookkeeping arm deleted by W4.3
    (`research/vcfs-respine/README.md`, `B3-NAMESPACE-PUBLICATION.md`, `D2-*.md`).

---

## 4. What the research/ directory investigated and concluded

All of `research/` is vcfs-era (2026-05 to 2026-07-22), i.e. the file-level design that lost.
Its value here is the write-path and durability measurements that transferred into the block
design, and the reasons file-level lost.

- `research/vcfs-write-path-analysis.md` (2026-05-13, FUSE era): a warm `stat()` cost 5 ms
  because of a mutex convoy across 5-7 locks per stat, a flush path releasing locks between
  per-file CAS splices, FUSE TTL 1 s as multiplier. Write bursts froze 5-20 s. Kernel time in
  strace was 0.001 s per 60 s: the cost was all userspace locking.
- `research/vmfsd-stall-deep-research.md`: a guest write+fsync took 20-43 s with 58 vmfsd
  processes on 128 cores. Cause: `block_on()` on the single vring worker thread, 7,714 threads
  starving under CFS (effective granularity 6 ms, period up to 1.2 s), a missed-kick race, and
  non-PI mutex inversion. ZFS exonerated (kernel fdatasync never above 4 ms). Fix: one thread
  per queue, non-blocking dispatch, tokio workers capped 2-4, cgroup isolation.
- `research/virtiofs-dispatch-research.md`: none of five reference virtiofs backends block the
  queue thread; the virtio spec permits out-of-order completion. virtiofsd pool 64 -> 1 thread
  gave 2,222 -> 2,622 IOPS; Kata DAX 3.6k vs 252k IOPS.
- `research/vcfs-respine/README.md`, W1-W3: 35.6-47% of worker self-CPU was observability
  (gating spans cut warm CPU 58%, wall 46.6%; armed tracing still costs 7.73% wall); 59% of
  channel requests were bookkeeping, 93% answerable from RAM; lookup median 21.5 us -> 1.96 us;
  cold metadata walk 24.8 s -> 1.17 s. The stall verdict: 70.1% of stall time was off-CPU
  runnable, 191 of 205 preempted stalls lost the core to another VM's worker; 13 VMs x 63
  workers on 32 CPUs = 25.6x oversubscription; routine 2-8 ms stalls, "not a VCFS defect".
- W4 ABI v4: 128-byte wire entries (64-byte entries had zero free bytes), guest-assigned
  handles, three timeout clocks (60 s queue / 30 s op / 120 s barrier), LSN on completions.
  Zero completion timeouts in 10 M+ requests, so idempotency dedup was reserved, not built.
- W5 write plane: mutable 64 KiB builders in RAM, chunk once at seal, vectored writeback. Per-
  write CDC + RMW had cost ~6x write amplification (`dirty_density_ppm=157443`). W5 cut write
  requests 76% and write busy time 92% on the fastfetch workload (`vmfs/vcfs/profile-artifacts/2026-07-18-w5-fastfetch-perfetto/README.md`).
- W6 / BARRIER-FLOOR: measured the replicated durability floor (section 2.1). Conclusions:
  parallel owner fences beat serial at every K>1; one slow owner sets the tail; an all-owners
  ack makes the SLO hostage to the slowest owner; owner "durable" was page cache.
- W9 intent log: the design the block volume inherited nearly verbatim (section 1.4). Forcing
  data: log absorbs 56-130x faster than CAS drains; crash-loss window was all staged RAM plus one
  fold, 6-20 s of acked writes; an idle volume parked 225 MiB in the crash window, so the idle
  trigger forces a checkpoint. Post-W9 pressure run (`vmfs/.../2026-07-22-w9-pressure/README.md`):
  fsync-per-op 4,172 IOPS (was 413); in-guest fsync avg 466 us / p99 1.6 ms / p99.95 23.5 ms /
  max 145 ms (was p99.95 9.6 s); sustained ingest ~329 MB/s (was 13.4); checkpoint debt avg
  46 MiB, peak 421 MiB; 0 poisoned / 0 dropped across 152,343 flushes.
- W5W6 negative lookups, B3, D2, SECOND-LOOK: coherence of negative dentries needs an
  invalidation ring; delta-sized work at the completion boundary regressed under A/B; persistent-
  map snapshots published per mutation cost ~12% CPU (`im::HashMap` clone storm).
- Why file-level lost (inferred across the corpus, no single file states it): 59% of requests
  were POSIX bookkeeping; nix canonicalization forced ~4 round trips per created inode that no
  protocol could batch (D2); every coherence question (negative dentries, xattr bits, attr
  authority, orphan fences, dir-stream budgets) needed a bespoke wire feature; 32 s of a 77 s
  benchmark was guest work; fsync honesty needed the WAL the design had excluded.
- `vmfs/vcfs/profile-artifacts/*`: the Perfetto evidence behind the above. Notable: the W4 fix
  decoupled completion-ring slot ownership from page ownership because golden restore starts a
  fresh channel epoch and the old coupling deadlocked; `OPEN_READ_INLINE` clobbered guest-dirty
  `i_size` 1000/1000 (the writer's cache is the authority between commits).
- `hardware/`: vendor datasheets with no ix annotations (checked by grep); only `ovh/pricing.md`
  is ix-specific. `prompts/`: eight bare question prompts; the compare-* files assert nothing
  about the VMM or RPC beyond their existence.

---

## 5. Lessons for the research design

Each item: what ix did or learned (file), then what it implies for a vhost-user-blk daemon with a
local staging log, background compactor, rendezvous-placed chunk store at RF k, RAM cache, cold
TCP reads, map-move migration, and epoch GC.

### Durability and the staging log

1. ix learned that a local NVMe fdatasync is 35 us average / 78 us p99 and a replicated barrier
   is 3-6 ms p99, and that acking from RAM produced 9.6 s p99.95 fsyncs once the drain fell
   behind (`W9-intent-log.md`, `BARRIER-FLOOR.md`). For the research system: local-fdatasync ack
   is the right tier one, and the paper can quote the two-orders-of-magnitude gap as the reason.
   Ack after the log's fdatasync only; never after a page-cache write.
2. ix's group commit flushes immediately when a waiter exists and every 50 ms otherwise, because
   a 35 us fdatasync makes any batching delay slower than the sync (W9 D6; WS-LOG). Implies: do
   not add a linger. ix later measured a 500 us LMDB commit linger wasting 97% of its windows.
3. The FLUSH watermark rule: fold each completed write's LSN into a per-volume `fetch_max`
   BEFORE the completion is visible to the driver; FLUSH covers `max(requested, barrier)`; the
   ordering is the correctness argument and the test needs a negative control (WS-LOG sections
   1, 4). Implies: with multi-queue vhost-user-blk the same obligation holds across queues; put
   the watermark in the daemon, test the reorder, and note virtio-blk has no FUA so FLUSH is the
   only durability verb.
4. Mint the LSN and append inside one critical section so log order equals LSN order, or replay
   can invert last-write-wins (WS-ENGINE FINDING 1). Implies: the staging log's append and the
   in-RAM index update must be atomic with respect to each other; replay must be LSN-ordered.
5. An empty discard acking an unassigned LSN wedged the next FLUSH; virtio-blk has no timeout
   handler so the guest sat in D-state (WS-LOG FINDING 3; design/03). Implies: every mutating
   verb, including no-op trims, must ack an LSN the log already covers; and the stall model for
   the guest is "hangs patiently forever", so daemon liveness is the whole availability story.
   Pin `hung_task_timeout_secs` above the daemon restart window.
6. A poisoned log must fail the barrier with IOERR, not fake a watermark (WS-LOG FINDING 2).
   Implies: define the append-failure path explicitly; the honest fallback is a demand compaction
   plus fdatasync of the chunk store.
7. Torn-tail recovery must handle a preallocated segment (partial record followed by zeros,
   exits via CRC) not just a shortened file (WS-LOG "Review pass"). Implies: test both shapes.
8. The log's crash window on an idle volume was 225 MiB until an idle trigger forced a
   checkpoint (W9). Implies: the compactor needs an idle trigger as well as size and dirty-entry
   triggers.

### The compactor (fold)

9. Latest-per-block coalescing before chunking cut CAS traffic 16-33x on an overwrite storm; the
   ratio is the whole economic case for folding late (WS-ENGINE Gate 2). Implies: the compactor
   should coalesce the staging log by block and chunk the coalesced image, never chunk per write.
   Per-write CDC + RMW cost ix ~6x write amplification (W9).
10. Packing extents in LBA order rather than arrival order made the map 3x smaller because
    leaf pages with arithmetic keys and few distinct hashes compress ~10x while blake3 digests
    do not compress (WS-MAP FINDING 9). Implies: compact in offset order; report map bytes per
    dirty MiB.
11. Chunk boundaries must be snapped to the block size when an extent may name only one chunk,
    or the corruption is silent (WS-ENGINE FINDING 2). Implies: either snap CDC boundaries to
    4 KiB or let an extent span chunks; decide before the map format is frozen.
12. Sub-block writes defer RMW to the compactor because resolving the missing sectors means
    reading the chunk store, which the write path may not wait on (WS-ENGINE). Implies: keep a
    sector mask in the staging index; the write path never reads cold data.
13. The compactor must never hold anything a FLUSH needs; ix asserts it at runtime and measured
    every flush under 100 ms with the chunk store slowed to 1 s per put (Gate 5). Implies: one
    compactor in flight, no shared lock with the FLUSH path, an assertion in the daemon.
14. Discard turned the compactor's write-only path into read-then-write and made discard cost
    proportional to extents covered (WS-ENGINE IN-PROGRESS 2; design/08). Implies: budget the
    discard path separately and say so in the paper; a 1 TiB `write_zeroes` is a constant-size
    map edit, a 1 MiB discard over fragmented extents is not.
15. Freezing the dirty index inside the pause window is O(dirty entries) and superlinear (79.5 ms
    at 1 GiB dirty); ix capped dirty entries at 65,536 (later 32,768) to bound it (Gate 2b, 2c).
    Implies: bound the staging set by entry count, not bytes, if snapshots or migration freeze it
    under vCPU pause; or use a persistent map for an O(1) freeze.
16. State the write-amplification denominator. ix's fold number is CAS bytes / coalesced bytes;
    total physical adds the log leg; a helper that counted only one leg reported 0.10x for a
    1.64x volume (WS-ENGINE). Implies: the paper reports log bytes + chunk bytes per guest byte,
    and the fold's own ratio separately.

### The map and snapshots

17. Snapshot = root hash at an LSN cut; branch = new head, same root; diff = O(delta) node
    visits (10 page fetches for 1 changed extent in 2,000) (spec section 5; WS-MAP). Implies:
    the offset-to-hash map should be a persistent content-addressed tree, not a flat table, if
    migration and GC want O(delta); a flat 100 GiB map at 4 KiB granularity is ~231-653 MB.
18. The map cannot hold device capacity; persist it beside the root or a shrink-then-grow
    resurrects old bytes (WS-MAP FINDING 6, 7). Implies: the manifest carries geometry and the
    open path validates it.
19. The cut is the FLUSH completion watermark, taken after the block queues are quiesced and
    before RAM is captured; disk ahead of RAM is a journal replay, disk behind RAM is corruption
    (inventory invariants). Implies: for a stock-QEMU vhost-user-blk daemon the snapshot cut must
    be driven from the VMM's pause (QEMU `stop` then a daemon RPC to freeze the index), and the
    daemon must expose "quiesce and return the watermark" as a verb.
20. `pause_blk_queues` reached 4.77 s of a 5 s abort budget because the quiesce served queued
    writes each paying a governor sleep; the fix bypasses pacing inside the fence (M3, U1).
    Implies: any backpressure sleep on the write path must be disabled while the guest is paused.
21. The capture RPC returns after metadata only; fold and replication are async and an artifact
    has three states (pinned / folded / ready) with two distinct stall diagnoses (watermark
    contract 4.5). Implies: separate "cut taken", "compacted", "replicated" in the design and
    the evaluation.

### The chunk store, placement, and replication (RF k across two hosts)

22. The replication drain was a software ceiling at ~50 MB/s = 0.86% of a 50 Gb/s link, set by a
    pacer holding its lock across its own sleep plus a per-page barrier, and reconfirmed by three
    unrelated workloads (M1-M3; drain-classes RC3, RC4). Implies: pipeline pushes per peer, never
    hold a lock across a sleep, and measure link utilization as the first sanity check.
23. Per-intent processing cost bound the drain (~1,400/s even at 91% dedup hits with zero wire
    bytes), and bytes per intent were workload dependent (60.5 KB bulk vs 17.3 KB OLTP) (M2).
    Implies: account replication per manifest or per batch, never per chunk; and evaluate with
    an 8 KiB-page database workload, not only dd.
24. Peer put acks were "indexed under MDB_NOSYNC", not stable; the disk-level ack costs one peer
    group commit (~50 ms) of watermark lag and zero throughput because throughput is set by window
    depth (watermark contract 5). Implies: two ack planes: a memory-level transport ack for flow
    control, a disk-level ack for the durability watermark; never delete the local pin on the
    memory ack.
25. `ready` (source deletable) is peer-disk, DRBD protocol C, because a single peer crash after
    source deletion would otherwise lose data (design-v0 10.2). Implies: with k=2 across two
    hosts the "replicated" predicate must be fdatasync-confirmed on the remote host before the
    local staging entries or GC epoch may advance past it.
26. Head-of-line blocking: intents keyed by hash are uniformly random with respect to urgency;
    an 89k-intent base-image seed delayed a first boot by more than its 8 s budget and a 64 s
    golden capture (drain-classes 1.6, RC1). Implies: declare a class on the write (bulk vs
    steady vs interested), reserve capacity per class, and never express urgency by cancelling
    in-flight work.
27. Backpressure that parks the guest is a bug class, not a tuning problem: M2 froze a 610-TPS
    guest 7 m 40 s; the redesign bounds by local disk and shapes latency per queue (design-v0
    0.5, section 6). Implies: the staging log's bound is local disk; if replication falls behind,
    the checkpoint may advance past the watermark with unreplicated chunks pinned locally, and
    the guest sees latency, never a stall. Say this as an invariant in the paper.
28. Write amplification on the wire at RF~2 measured 1.76-2.03x; per-peer streams did ~19 MiB/s
    each with no straggler even at 99% disk (M1, drain-classes 1.3). Implies: with k=2 expect
    ~2x wire bytes per guest byte plus manifest traffic; per-peer parallelism, not per-peer
    speed, is the lever.
29. Cross-volume dedup created a watermark soundness hole: a chunk already present locally staged
    nothing for the new volume, so volume B could certify replication it never owned (watermark
    contract ADDENDUM 2). Implies: if the chunk store dedups across volumes, a dedup-omitted
    chunk is sound to omit only if it is present AND fenced remotely; otherwise record a
    dependency credit.
30. The intent row is the local eviction pin (watermark contract 8). Implies: the RAM cache and
    any local chunk eviction must consult "has this chunk been fenced to its remote owner"
    before dropping the last local copy.
31. A stalled manifest that pins a contiguous watermark names the fault; holey accounting would
    not accelerate anything because WAL truncation is a prefix operation (design-v0 10.1).
    Implies: contiguous per-manifest watermark advance, one counter per (volume, manifest).

### Cold reads, caching, first boot

32. The guest page cache is the cache; host caches are small and miss-oriented (256 MiB chunk
    cache, map-node LRU); no host write-back cache because the log is the write cache (spec
    section 7). Implies: size the per-host RAM cache for cold-read reuse and map walks, not for
    hits the guest already holds; report what it actually buys.
33. Boot-as-restore with lazy blocks plus a per-image prefetch list is the EBS fast-snapshot-
    restore shape (spec section 7). Cross-node restore failed on a 120 s eager full-RAM preload
    against a 30 s deadline, "sixth instance of the readiness-seam class" (`12-decisive-run.md`
    leg e). Implies: never make readiness wait on a bulk fetch; serve reads on demand and
    prefetch behind them.
34. Seeding a base image into a node emitted 89k replica intents per node and was misdiagnosed
    for weeks; the fix was a region-durable stamp so image chunks never enter a per-VM
    replication pipeline (M1, design-v0 section 8). Implies: base-image chunks are pre-placed
    per host at deploy time and excluded from the per-volume replication accounting.
35. Base images must be pre-chunked artifacts with a manifest root; seeding from a raw file
    produced manifests that did not describe the disk (WS-IMAGE follow-up). Implies: the image
    pipeline emits chunks + map, and a volume's map references base chunks directly (ancestry
    sharing is the near-term dedup win).

### Migration by moving the map

36. What crosses the wire for disk is a root hash, a cut LSN, and an incarnation; the map is
    already in CAS and the target faults chunks from the still-alive source (contract 3.2;
    design-v0 section 7). This is exactly the research design, and the measured blackout was
    orchestration, not bytes: 8-10 s then 2.89-3.02 s with under 7 MiB moving, with the disk cut
    itself 3-6 ms (contract section 1; `02-disk-cut-dark-window.md`). Implies: the paper's
    migration claim should be about the control path (one round trip in the dark window) and the
    residual fetch, and the evaluation must decompose blackout into cut, device state, decision,
    and resume. Compute blackout and network blackout are different numbers; report both.
37. The publisher's steady 5 s folds during precopy made the terminal fold a replay
    (`rounds=1 tail_bytes=0`, 3-5 ms) (`02-disk-cut-dark-window.md` section 2). Implies: keep the
    compactor running normally during precopy; a separate "drain rounds" loop is unnecessary
    and, if it waits on replication with the guest paused, harmful.
38. Ownership handoff: final head publish + single-use successor marker in one fenced UPDATE;
    the target claims with a conditional UPDATE that clears the marker; the source's take-back is
    the mirror UPDATE on the same epoch; Postgres serializes them (contract section 4). Implies:
    the offset-to-hash map move needs a writer epoch and a single-use marker in whatever
    metadata store holds volume heads; the decision must be one conditional write, and every
    resume must be preceded by a successful write naming that node.
39. Startup adoption on the source inside the marker window stole the fence back and broke the
    claim; boot resolution must not read handoff columns (`02-disk-cut-dark-window.md` 4.3;
    contract 6.1). Implies: separate "who may acquire" (marker) from "which head is safe to open"
    (durable evidence) and test restart injection on both sides.
40. The process that reads the offer must be the process that can commit the decision; the
    postcopy fetch loop deadlocked when asked to also carry the cutover because a paused guest
    faults on nothing (contract 3.1.1). Implies: the cutover channel is dialed early, kept warm,
    and owned by the daemon or control agent that holds the metadata credentials, not by the
    page-fault path.
41. Readiness must be a positive durable signal written by the target, and the source's pause
    must not precede it; a silent target left a guest 607 s dark (contract 3.2.1). Implies: gate
    the freeze on the target's ack and put a deadline strictly shorter than the sweep on every
    in-window step, with values from a p99 measurement, not guesses.
42. Source teardown waits for replication of the final cut; the cut never does (design-v0 section
    7; `02-disk-cut-dark-window.md` section 3). Implies: with k=2 the old host may delete its
    staging log only after the remote fence covers the cut LSN.

### GC and epochs

43. GC's refcount edge set is `direct_child_hashes`; an extent's chunk hash not registered there
    means GC frees a chunk a live snapshot reads (WS-MAP). A "can't-happen" refusal in code that
    walks CAS generically "would free chunks under a live block volume" (parallel plan, pre-main
    ledger). Implies: the epoch GC must enumerate every reference kind (map nodes, data chunks,
    base image roots, staging pins) and fail closed on an undecodable root.
44. With CAS GC disabled fleet-wide, one node reached 1.8 GiB free of 1.7 TB, `ix rm` freed
    nothing, and image rebuilds (~16 a day) accumulated blob sets nobody reaped
    (`12-decisive-run.md`; WS-IMAGE). Implies: the paper's GC is not optional infrastructure;
    measure reclaim under snapshot churn and image churn.
45. A failed capture must leave the next delta a strict superset (QEMU bitmap rule), and every
    export is paired with a targeted release or later exports are blocked for the VM's life
    (design-v0 section 1; inventory invariants). Implies: epoch pins are released by exact
    (root, lsn) pair on every exit path, including reaper paths after a daemon restart.
46. A pinned-but-unfolded artifact surviving a daemon restart is a new orphan class that needs a
    reaper from day one (watermark contract W-CAP-RESIDUE). Implies: enumerate residue classes
    per state transition when designing the epoch scheme.

### Measurement discipline the reports converged on

47. Seven samples per point, judged on the worst, because a pause budget is not an average; one
    sample per point produced a non-curve that would have passed the assertion (WS-ENGINE Gate
    2c).
48. Iso-methodology or nothing: same node, same day, same guest with a flag flipped (WS-VMM
    queue-depth sweep design); a queue-depth sweep with `direct=1` for transport cost, `direct=0`
    for user-visible throughput, never conflated.
49. A test or gate must be shown able to fail: the oracle printed PASS unconditionally, the drift
    gate compared a value to itself, the freeze fixture died at exit 126 inside the sandbox
    (WS-LOG, WS-IMAGE FINDING 11, WS-CORDON section 6).
50. Interface counters are not a replication proxy outside a known drain window; use the
    server's own byte counters (M1 anomaly, M2 notes). Journal greps undercount (100 ms filter);
    read the counter file.
51. Report "unmeasured" as a word, never as a rounded guess (WS-IMAGE header; contract
    C-DEADLINE-MEASURED). Every workstream report carried a NEEDS MEASUREMENT list.

### The one architectural fork the research design takes differently

ix runs the backend in-process in the VMM and pays for it with "crash-consistent, not restart-
transparent" and with fault injection through a stall file. The research design's vhost-user-blk
daemon is out of process by construction, which recovers restart transparency (the property
vcfs had and the block pivot lost, spec G3 caveat). What ix's evidence says that property costs:
the guest hangs patiently in D-state with no timeout, so the daemon's restart window is the
outage; replay of the staging log on reopen is the recovery path (2-5 s target in spec 9.1);
the migration cutover must then be split between the daemon (index freeze, watermark) and a
control agent holding the metadata credentials, because the daemon that serves guest I/O should
hold no control-plane authority (D05 Q3). The stall test ix designed (`IX_BLOCKVOL_STALL_FILE`,
in-guest sampler that touches no disk, refuses green on absence of evidence) becomes a real
`SIGSTOP` / `SIGKILL` on the daemon with the same assertions (WS-VMM "The design-03 fault test").
