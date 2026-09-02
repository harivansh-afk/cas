# Content Addressed Deduplication: A distributed storage system study

CS 4993, fall 2026. Research spec, v8.

This file mirrors `playbook/src/routes/00–06` for review and hand edits.
Edits here are ported back into the pages.
Figures are described in brackets where the pages draw them.

---

# 00 Thesis

**Thesis.**

A deduplication table shares duplicate data within one host.

Content addressing shares it across hosts.

On Linux VM fleets the first is already solved by ZFS.

So the case for content addressing rests on what a name does that an address cannot: move only unique bytes between hosts, store each chunk k times across a fleet instead of once per host, and serve a chunk from whichever host holds it in memory.

This study builds that backend on a stock hypervisor and measures what each provides and what each costs.

## Where deduplication stops

Two VMs each run `apt upgrade` and download the same packages.

Their disks now hold the same bytes.

No clone can share them, because neither copy descends from the other.

A deduplication table can.

ZFS deduplication and dm-vdo hash every block and share equal ones, at a fixed aligned block size.

On a Linux guest everything is 4K aligned: ext4 uses 4K blocks, partitions start at 1 MiB, and package managers write whole files.

So at 4K a deduplication table reaches nearly all of it, which is why Jin and Miller found fixed blocks match content-defined chunking on VM images in 2009.

**On one host, content addressing has no capacity win over ZFS at 4K.**

Part 1 measures this instead of assuming it.

Every local-disk mechanism stops at the host boundary.

The DDT is per pool.

`zfs send` dropped deduplicated streams in 2.0.

dm-vdo has no replication.

Reflinks do not survive rsync.

A clone on host B shares nothing with host A.

A fleet of N hosts each running deduplication stores every shared chunk N times and moves it whole every time a guest moves.

[figure: left, two hosts each with a DDT and no link between them, shared chunk stored twice, migration moves the full logical size; right, two hosts whose chunks are owned by hash, one namespace, chunk stored k times, map moves and chunks stay, cold read fetches by name from the owner]

## What content addressing provides

A chunk named by its hash has the same name on every host, so placement is a function of the name.

Provisioning a guest moves its map, 32 bytes per chunk, and no chunks.

Migrating a guest moves the map plus whatever it wrote since the last compaction.

A fleet stores each chunk k times, not once per host.

A chunk that is hot anywhere is in some host's memory, and **a peer's memory over 100 GbE is closer than local NVMe**: about 20 µs against about 80.

## The cost

The network sits on the read path for cold chunks and nowhere else.

Never on the write path, never on FLUSH.

Part 3 measures the cost of a cold read on TCP and on RDMA, from a peer's memory and from its NVMe, and shows how much of it prefetch hides.

The remaining costs are the ones every deduplication design incurs, and this one measures them: write amplification, compactor interference with the guest, index memory, and the window between a local ack and the chunk being durable on its owner.

## Hypotheses

**H1. Single-host parity.**
The daemon stores within 10% of the bytes ZFS fast dedup stores at the same block size, with guest p99 within 20% of a raw file on XFS.
Index bytes per TB fall in inverse proportion to chunk size.

**H2. Cross-host benefit.**
Provisioning and migrating a guest between hosts move the map plus the uncompacted tail, within 10% of that bound.
With one copy per chunk, the two-host testbed stores at most 55% of what two per-host deduplication stores hold.

**H3. The cost is the remote cold read.**
A chunk served from the owner's memory arrives faster than a local NVMe read on both TCP and RDMA.
From the owner's NVMe it costs at most 30% over local on TCP and 15% on RDMA.
With enough reads in flight, remote sequential throughput matches local.

Thresholds come from the transport literature on page 04 and the census prediction on page 02.

They are frozen at the end of week 2 and do not move.

## What comes out

- A working content-addressed block backend under unmodified QEMU, on a stock kernel, over kernel TCP.
- A single-host table against ZFS fast dedup: capture, p99, write amplification, index memory, as a function of chunk size.
- Two numbers no existing backend can match: bytes moved to provision and migrate a guest, and fleet bytes stored with one copy per chunk.
- The first microsecond-scale measurement of a content-addressed chunk fetched from a peer under a VM block device, over kernel TCP and over NVMe-oF on TCP and RDMA.

## Scope

A1. Hosts serving guests from local flash, homelab to rack scale. Array economics are out of scope.

A2. The design places chunks over N hosts by rendezvous hashing; the testbed is two hosts with static membership. No failure detection, rebalancing, or authentication. One copy per chunk on two hosts is a measurement configuration; a deployment runs k ≥ 2 on N ≥ 3.

A3. One image, one writer. Disk migration only; memory migration is QEMU's.

A4. The guest contract is virtio-blk with a volatile write cache: an acknowledged FLUSH is durable and nothing else is. Every configuration runs with QEMU <code>cache=none</code>, so the host page cache is bypassed everywhere.

A5. Equal BLAKE3 implies equal bytes. A sample of matches is verified byte for byte and the sample size reported.

A6. The store is trusted infrastructure. Deduplication side channels are documented and excluded.

A7. Experiments run at single-digit TB. Larger figures are formulas with measured constants and are labeled as such.

A8. RDMA is a measurement arm on page 04. Nothing in the architecture requires it.

---

# 01 Architecture

**Invariant.**

The network is on the read path only, only for cold chunks, and never on the write or flush path.

Every design choice below follows from it.

## One host

The guest sees a virtio-blk device on stock QEMU.

QEMU connects it over vhost-user-blk to one process per host, the daemon.

All new code lives there.

Guest memory is shared with the daemon, so requests are read in place, and storage IO goes through io_uring.

[figure: guest → stock QEMU → vhost-user → daemon. Inside the daemon: staging log (append-only, local NVMe, FLUSH → fdatasync → ack); compactor (fixed 4K or FastCDC, BLAKE3, owner = rendezvous(hash)) fed by settled extents; owner = self → local store; owner = peer → PUT to owner, batched, durable ack, then mark compacted; index (hash → offset, RAM, rebuildable); chunk cache (hash → bytes, RAM, bounded); maps (offset → hash, one per image). Read: staging → local store → cache → GET(hash) from owner.]

## Write path

Guest writes append at block granularity to a staging log on local NVMe.

FLUSH is `fdatasync` of the log, then the acknowledgment.

The hot path hashes nothing and chunks nothing, so large writes proceed at sequential-append speed.

Durability belongs to the log alone.

The page cache never holds the only copy of anything, and every file is opened O_DIRECT.

Staging is finite; when ingest outruns compaction, back-pressure throttles the guest, and the point where it engages is measured.

## Compactor

A background pass reads settled extents from staging, cuts them into chunks, hashes each with BLAKE3, and discards any hash already in the local index.

Chunking is fixed 4K or FastCDC, chosen per arm on page 02.

Extents overwritten in staging are never compacted.

For each new chunk, the owner is the first k hosts in rendezvous order of its hash.

If the owner is this host, the chunk is appended to the local store and written with fdatasync.

Otherwise it goes in a batch to the owner, which appends, fdatasyncs once per batch, and acks.

**Only after the ack does the extent count as compacted.**

Staging is the write-ahead log for the whole fleet.

Two costs come with this and both are measured.

Every surviving byte is written at least twice, staging then store, plus journal traffic.

Compaction reads and writes the same device the guest is using, so guest p99 is measured with the compactor active and idle.

CDC over a dirty extent re-chunks from the last settled boundary before it to the first boundary after it that agrees with the existing cut.

This is the standard resynchronization rule (LBFS locality; Xet's boundary reset), and it is why CDC never runs on the hot path: one aligned write can move every boundary in its neighborhood.

## Read path

Reads check staging, then the local store, then the chunk cache, then send `GET(hash)` to the owner.

The owner answers from its cache if the chunk is hot, otherwise from its store.

Fresh data is served without indirection; settled data incurs the map walk, the index lookup, and, if the owner is remote, one round trip.

The chunk cache is daemon-owned memory keyed by hash, LRU, with a size that is a parameter.

Because every file is O_DIRECT, the kernel page cache holds nothing on any host, and the cache size is set equal to ARC on the ZFS configuration.

Prefetch is the daemon issuing the next D hashes from the map when it sees sequential reads, and optionally replaying a recorded boot profile.

D is swept on page 04.

## Capacity tier

The local store is an append-only log of records (length, hash, flags, bytes) and is authoritative for the chunks this host owns.

The index maps hash to offset, lives in memory, and is rebuilt by scanning the store; its bytes per TB is the constant the chunk-size arms measure.

The map, one per image, is a journaled offset tree from disk offset to chunk hash.

It lives with the guest's host and moves when the guest does.

## Protocol

| Message | Reply | Used by |
|---|---|---|
| GET(hash) | bytes | cold read, prefetch |
| PUT(batch of chunks) | ack after one fdatasync | compactor sending chunks to an owner |
| HAS(hashes) | bitmap of hashes the owner lacks | compactor before PUT, so only missing chunks are sent; provisioning verification |
| LIVE(epoch, hashes) | ack | garbage collection |

Length-prefixed messages over kernel TCP, one connection per core, `TCP_NODELAY`, driven by io_uring.

The daemon runs busy-polling or blocking; page 04 measures both, because the scheduler wakeup is part of the cost.

Rendezvous hashing means a reader already knows the owner of every hash; nobody looks up anyone else's index.

RDMA and NVMe-oF exports appear on page 04 as probes that show what the kernel stack costs.

The architecture does not depend on either.

## Placement and k

Owner set = the first k hosts in rendezvous order of the chunk's hash.

k is the one cross-host parameter.

With N hosts, k = N places every chunk on every host (replicated) and k = 1 places each chunk on exactly one (partitioned). On the two-host testbed these are k = 2 and k = 1.

Page 03 measures both; a deployment would run k ≥ 2 on N ≥ 3 hosts.

## Durability

| Failure | What survives | Against R0 and R1 |
|---|---|---|
| daemon crash | everything: replay the staging log, re-run incomplete compaction epochs | gate G2, `fio --verify` after `kill -9` |
| host crash, power loss | everything acked: FLUSH was fdatasync on local NVMe | same contract as a local disk |
| host lost | acknowledged bytes not yet transferred are lost; with k = 1, chunks it owned are gone fleet-wide | R0 and R1 lose everything too; the window is measured, and k ≥ 2 closes the second half |

Two rules follow.

Bytes are durable on local NVMe before they go on the wire, always.

Transfer is two-phase: the owner fdatasyncs and acks before the sender marks anything compacted or reclaimable.

The window between a local ack and the chunk being durable on its owner is the compaction lag, measured in seconds under the fleet replay.

One optional arm closes it: mirror the staging tail to the peer on every FLUSH and wait for its fdatasync before acking.

Every production system in this space does this, and the arm measures its cost: one round trip per FLUSH.

## Crash consistency

Two logs, staging and the map journal, must agree after a crash.

Staging is senior.

Compaction is idempotent and every batch carries an epoch recorded in both logs.

Recovery replays staging, discards map records from any epoch whose extents were not marked compacted, and re-runs compaction from the oldest incomplete epoch.

`kill -9` at any point, then this replay, must pass `fio --verify` before any number from the daemon is reported.

## Garbage collection

A chunk is live if any staging log or any map on any host references it.

Each host sends its owner the live set for an epoch with `LIVE`; the owner sweeps with `FALLOC_FL_PUNCH_HOLE` over dead records.

No reference counts.

The sweep runs once after the fleet replay to report reclaimed bytes; concurrent collection is out of scope.

## Out of scope

Membership changes, failure detection, rebalancing when a host joins or leaves, authentication and encryption on the wire, measurement on more than two hosts, and concurrent garbage collection.

Each is named in future work on page 05, and none of them affects a number this study reports.

## Provenance

| Component | Source | License |
|---|---|---|
| hypervisor | stock QEMU, unmodified, vhost-user-blk front end | GPL-2.0 |
| vhost-user protocol | rust-vmm `vhost-user-backend`, `vm-memory`, `virtio-queue`; Cloud Hypervisor's `vhost_user_block` read as reference | Apache-2.0 / BSD-3-Clause |
| hashing | `blake3` crate | CC0 / Apache-2.0 |
| chunking | `fastcdc` crate | MIT |
| host filesystem | XFS on the dedicated NVMe, O_DIRECT, hole punching; ZFS never sits under the daemon | |
| staging, compactor, store, index, maps, cache, protocol, garbage collection | this study | new code |

Because the hypervisor is unmodified, no result can be an artifact of a patched QEMU, and the raw-file control runs the identical binary.

---

# 02 One host

**Part 1.**

Same stock QEMU, same guest, same NVMe device, storage behind the device varies.

The prediction is a tie on capture between the daemon and ZFS fast dedup.

It is measured anyway, because the chunk-size curve under it is the single-host design result, and because the comparison against ZFS is the first objection a reviewer will raise.

## Configurations

**R0. Raw file on XFS.**
QEMU's raw driver on the dedicated NVMe.
The control; no deduplication anywhere in the path.

**R1. Zvol on ZFS 2.3 fast dedup.**
Own pool on the same device, created and destroyed per run, opened by QEMU as a block device.
`feature@fast_dedup`; `dedup=blake3`, since `dedup=on` silently uses SHA-256 regardless of the checksum property; `volblocksize=16K` primary and `4K` second arm; `compression=zle` outside the compression arm so zero blocks do not collapse onto one DDT entry; `dedup_table_quota` unset and `zpool ddtprune` never run during a measurement; DDT memory from `zpool status -D`.
OpenZFS direct IO does not apply to zvols or with deduplication enabled, so R1 is ARC-backed in every arm, and the paper reports it as such.

**R2. Raw file on XFS over dm-vdo.** (optional)
Inline fixed-4K deduplication in the kernel, mainline since 6.9.
Its own XFS instance on the vdo device.
Index memory from `vdostats`.

**R3. The daemon, one host.**
Local store only; k does not apply.
Three chunk-size arms below.

R0 against R3 is the cost of the daemon with everything else held constant.

R1 is the deployed state of the art and differs in kernel boundary, caching, and allocation, so it is a case study beside the controlled pair, and the paper attributes deltas accordingly.

## Chunk size

Fixed 4K captures everything a Linux guest offers, and costs an index entry per 4K: about 250 million entries per TB, roughly 10 GB of memory per TB at 40 bytes each.

That is the DDT memory cost the daemon is designed to avoid.

FastCDC at a 16K mean cuts the index four times over and loses some aligned matches.

Three arms: fixed 4K, fixed 16K, FastCDC 8K to 64K with a 16K mean.

Reported per arm: bytes stored, index bytes per TB, guest p99, write amplification.

**Capture against index memory as a function of chunk size is the result this page produces.**

The census below predicts the capture column before any run.

## Workloads

- fio: 4K random write and read at QD1 and QD32; 128K sequential.
- Boot storm: N clones of one image booted together, N = 4, 16, 32.
- Fleet replay: the synthetic fleet below written onto N guests, at two points on its timeline.

No kernel build and no synthetic stress workload that exists only to exercise the daemon.

## Metrics

- Guest p50 and p99 write and read latency against R0, compactor active and idle. Reported first.
- Bytes stored after compaction completes, against the census prediction at the configuration's block size.
- Index or DDT bytes per stored TB.
- Write amplification: device bytes written per guest byte, from NVMe counters.
- Sustainable ingest and the back-pressure point.
- Recovery: `kill -9`, replay, `fio --verify`.

## Controls

Pinned vCPUs, performance governor, discarded warm-up, fresh filesystem or pool per repetition, at least five repetitions, variance beside every number.

With `cache=none`, R0 and R2 have no host cache; `zfs_arc_max` on R1 and the daemon's cache size on R3 are set equal.

All configurations are observed at the guest boundary (fio's histograms, guest-side blktrace for the boot storm) plus host device counters.

The daemon adds per-request stage timestamps drained to ndjson, cross-checked once against bpftrace with the delta reported.

`zpool` and `vdostats` figures are supplementary.

## Prediction

A small census supplies two numbers the rest of the study is measured against: how many unique bytes a fleet holds at a given block size, and how many bytes copy-on-write would already have shared.

**Phase 0.**
`zdb -S` on a ZFS pool holding the cloned fleet.
Pool traversal starts each dataset at its previous snapshot's txg, so blocks a clone inherited from its origin are counted once, and the simulated ratio is duplicates beyond what clones already share.
Verified in `dmu_traverse.c`; confirmed by a five-minute test before it is cited.

**The fleet.**
Ubuntu publishes dated cloud images and snapshot.debian.org serves the archive as of any date.
An image installed as of T0 and upgraded monthly against the archive as of T1, T2, and on replays a real update history.
N such clones with scripted drift (hostnames, logs, a few packages each) form the fleet.
It is rebuilt by one command, dated, and is also the replay workload above.

**The split.**
Per byte range: zero or unallocated (from the guest allocation map, excluded), unique, shared with the T0 base in place, duplicate at an aligned 4K or 16K boundary elsewhere in the fleet, or duplicate only at a shifted offset.
The aligned column predicts R1 and the fixed arms; aligned plus shifted predicts the CDC arm.
Nothing further: no donors, no real fleets, no claims about time.

---

# 03 Multiple hosts

**Part 2.**

Same daemon, N hosts, one parameter: k.

Every number on this page is one no local-disk backend can match.

## Two modes

k is the number of owners per chunk.

The design supports any N; the testbed has two hosts, so k takes two values, and they are two different experiments.

[figure: left, replicated k = 2: host A and host B each hold all chunks, PUT new chunks both ways, each unique chunk crosses the wire once, every read is local, capacity = one store twice. Right, partitioned k = 1: host A holds chunks with hash → A, host B holds hash → B, PUT and GET both ways, each chunk stored once fleet-wide, about half of cold reads are remote, capacity = one store once.]

k = 2 provides transfer savings and keeps every read local.

k = 1 provides capacity savings at the cost of remote reads.

Two hosts with k = 1 is the worst case for remote reads and is run for exactly that reason.

## Provisioning

A new guest on host B from an image whose chunks exist anywhere: copy the map, 32 bytes per chunk, about 80 MB for a 40 GB image at 16K chunks. Every chunk it names already exists at its owner.

In replicated mode no other data is transferred.

In partitioned mode no other data is transferred either; chunks are fetched on first read.

**Provisioning cost is the size of the map.**

Baseline: `qemu-img convert` or `scp` of the raw file, and `zfs send | zfs recv` of the zvol, each moving the allocated size of the image.

## Migration

Move a guest from A to B: stop, copy the map and the staging extents not yet compacted, start.

A 40 GB guest that compacted recently moves in tens of MB.

Memory migration is QEMU's and is out of scope; this is the disk.

Baseline: rsync of the raw file, `zfs send` of the zvol.

Since 2.0 `zfs send` emits no deduplicated stream; the bytes are the logical size regardless of the DDT.

## Synchronization after drift

Two guests, one on each host, cloned from the same image, each updated independently to the same package set.

Compaction on each host sends only the chunks the owner lacks.

Bytes on the wire are read against the census's unique-byte count for the pair.

This is the `apt upgrade` case from page 00, measured.

## Capacity

Partitioned mode stores each chunk once across the fleet.

Measured: bytes on both stores after the fleet replay completes, against two per-host ZFS pools holding the same guests.

Predicted: about half.

Also measured: what fraction of a guest's cold reads went to the other host, which on two hosts with k = 1 should be about half and is the worst case any fleet would see.

## Durability window

Between a local FLUSH ack and the chunk being durable on its owner sits the compaction lag.

It is reported in seconds under the fleet replay, as a distribution, with the compactor's transfer batch size as the parameter.

Optional arm: mirror the staging tail to the peer on every FLUSH and wait for the peer's fdatasync before acking.

Every production system in this space does this.

The arm reports the write p99 it costs on TCP, which is one round trip per FLUSH.

## Measured

| Flow | Daemon | Baseline | Read against |
|---|---|---|---|
| provision | bytes transferred, both modes | scp of raw file; zfs send | map size |
| migrate | bytes transferred, both modes | rsync; zfs send | map size + staging tail |
| sync after drift | bytes sent by compaction | rsync; zfs send | census unique bytes |
| capacity | bytes stored, partitioned | two per-host ZFS pools | census prediction |
| remote fraction | cold reads served by the peer | | about half, worst case |
| window | seconds from ack to owner-durable | mirror arm: write p99 with mirroring | one RTT per FLUSH |

## The locality objection

Dong et al. (FAST '11) rejected per-chunk hash placement for backup because it destroys read locality and routed 1 MB super-chunks instead.

This is primary storage with a local cache, and the fragmentation cost they argued about is measured directly on page 04 instead of argued.

If it is large, placement by super-chunk is the knob, noted here and measured only if time remains.

---

# 04 Remote read

**Part 3.**

A cold read whose chunk lives on another host is the only place the network enters guest latency.

This page measures it and reduces it.

## Where the time goes

Every read has about 80 µs of NVMe media time under it.

On 100 GbE the transport sits on top: raw RDMA adds 3 to 5 µs, kernel nvme-rdma about 12, kernel nvme-tcp about 21, a userspace daemon over kernel TCP 20 to 30.

Those are the SPDK 24.05 and Systor '17 numbers on ConnectX-5; the testbed replaces them.

RDMA against TCP is therefore a 10 µs difference on an 80 µs read.

The larger factor, about 4x, is whether the chunk is in the owner's memory or on its disk.

If those numbers hold, **a chunk from a peer's memory over TCP arrives faster than one from local NVMe**; H3 tests this.

With hash placement, a chunk shared across the fleet is hot at exactly one owner, and every host's read of it hits that owner's cache.

[figure: horizontal bars in µs from the literature. local NVMe ≈ 80. peer memory: RDMA ≈ 12, nvme-tcp ≈ 21, daemon TCP 20–30, all shorter than local NVMe. peer NVMe: RDMA ≈ 92, nvme-tcp ≈ 101, daemon TCP ≈ 110, 10 to 30% over local.]

## Probes

The architecture's transport is the daemon over kernel TCP.

The other rows exist to show what the kernel stack and the userspace hop each cost; nothing depends on them.

| Probe | What it isolates | Code |
|---|---|---|
| `ib_read_lat -s 4096` | the hardware floor | none |
| nvme-rdma export | kernel block path over RDMA; owner's store exported by `nvmet` as a file-backed namespace, `buffered_io` on for memory, off for media | configuration |
| nvme-tcp export | same over kernel TCP | configuration |
| daemon, TCP, busy-polling | the architecture, without the wakeup | the daemon |
| daemon, TCP, blocking | the architecture as deployed; the scheduler wakeup is the cost | the daemon |
| daemon, ibverbs two-sided (stretch) | the userspace hop without the kernel stack | ~40 h |

The nvmet export is a probe and not the architecture: it exposes the raw store, needs the reader to know offsets, and has no place for authentication.

It is in the table because the difference between it and the daemon over the same TCP is the cost of the userspace hop, with SPDK's 1 µs kernel-versus-userspace target delta as the reference point.

## Method

- Same two hosts, NIC, drive, and kernel for every row. Kernel, firmware, MTU, IRQ affinity, interrupt moderation, C-states, busy-poll, and PFC state recorded.
- Two targets per row: a null device for fabric plus stack alone, and the real file for end to end. Each from the owner's memory and from its NVMe.
- 4K, 16K, 64K. p50, p99, p99.9. Five runs of 30 s, caches dropped between, medians with spread.
- QD sweep 1, 4, 16, 64 for throughput and CPU per IOPS on both ends; TCP costs about 2.5x the CPU of RDMA at equal IOPS and the paper shows the ratio it measures.
- RoCE hardware counters (`out_of_sequence`, `packet_seq_err`, `local_ack_timeout_err`) printed beside every RDMA number, proving zero retransmits on a fabric with no PFC.

## Prefetch

The map tells the daemon what comes next.

Depth sweep: sequential reads through the map with 1, 2, 4, 8, 16, 32 chunks in flight, at 4K and 64K.

The bandwidth-delay point is about 250 KB for the fabric and about 1 MB with media under it, so roughly 20 chunks of 64K or 300 of 4K outstanding should hide the remote entirely.

Success is remote sequential throughput within the error bars of local.

Profile prefetch: record the chunk sequence of one boot, replay it on later boots.

Every lazy-loading system that has published numbers does this and reports it removing most of the miss cost; DADI says 95%.

It is the consensus mitigation and a one-day implementation.

## Under a guest

Partitioned boot storm at N = 16, with and without profile prefetch, against the same storm in replicated mode.

Reported: guest p99 and host device reads per guest byte.

**The gap between partitioned with prefetch and replicated is the residual cost of one copy per chunk.**

## RDMA is a probe

The CloudLab fabric is lossy; no PFC or ECN is documented on the shared switches, and published work on this node type ran RoCE that way.

Adaptive retransmission is enabled on the NIC and the counters above prove the runs were clean.

ConnectX-5 cannot do io_uring zero-copy receive, so that option is unavailable.

None of this touches the architecture, which runs on kernel TCP and would run on any Ethernet.

## H3, restated

- A chunk from the owner's memory arrives faster than a local NVMe read, on TCP and on RDMA.
- From the owner's NVMe it costs at most 30% over local on TCP and 15% on RDMA, at QD1, 4K.
- At depth at or above the bandwidth-delay point, remote sequential throughput is within 10% of local.
- Partitioned boot storm p99 with profile prefetch is within 25% of replicated.

---

# 05 Plan

**Fourteen weeks, about 320 hours.**

That is 23 a week.

The course credit corresponds to 8.

The plan is sized to the work, and the descoping order defines what is removed if it slips.

## Hardware

Two CloudLab c6525-100g nodes (Utah), reserved as a pair.

Per node: AMD EPYC 7402P, 24 cores at 2.80 GHz; 128 GB ECC DDR3-3200; two 1.6 TB PCIe 4.0 NVMe SSDs; ConnectX-5 Ex 100 GbE, one port on the experiment network.

One NVMe holds the system and results; the other is the device under test.

The pair is one hop through a single switch.

RoCE between two of these nodes works and has been used in published work on this exact hardware, on a lossy fabric.

Self-built kernels are routine there; the Ubuntu 24.04 image ships 6.8, dm-vdo needs 6.9, and OpenZFS 2.3 is a source build, so a kernel and ZFS are built once in week 1 and snapshotted as an image.

Reservations expire at 16 hours by default, so every run is scripted to complete inside one.

CloudLab is free for research.

A project is opened by a faculty member and reviewed by CloudLab staff; the sponsor opens it before Sep 9.

Fallback: two OVHcloud Advance-4 2026 servers (EPYC 4585PX, 16 cores, 64 GB DDR5 ECC, 2 × 960 GB NVMe) on a 25 Gbps private link, which loses the RDMA arm and replaces the 100 GbE fabric with 25 GbE.

## Schedule

| Weeks | Build | Measure |
|---|---|---|
| 1–2 | vhost-user-blk daemon in passthrough: staging log, FLUSH, replay. Kernel and ZFS image. | R0; passthrough within 10% of R0 p99 (G1). Thresholds frozen. `zdb -S` phase 0 on the synthetic fleet. |
| 3–5 | Compactor, store, index, maps, epochs, recovery. Three chunk-size arms. | `kill -9` recovery passes (G2). First capture numbers. |
| 6–7 | R1 configured, both volblocksize arms. R2 if time permits. | Part 1 table complete (G3). |
| 8–9 | Protocol, rendezvous placement, k, PUT with durable ack, HAS, single-pass garbage collection. Provisioning and migration scripts. | Replicated mode on two nodes. |
| 10 | Partitioned mode. Mirror arm if time permits. | Part 2 table complete (G4). |
| 11–12 | nvmet exports, RoCE configuration, busy-polling and blocking daemon, depth prefetch, profile prefetch. | Transport matrix and prefetch sweeps (G5). Partitioned boot storm. |
| 13–14 | | Report; reproducibility pack (G6). |

## Gates

G1. Passthrough daemon under stock QEMU within 10% of R0 p99 by the end of week 2. If this slips, everything after it slips, and the sponsor is informed that week.

G2. `kill -9` at arbitrary points, replay, `fio --verify` passes, before any daemon number is reported.

G3. Part 1 table complete: R0, R1 at two block sizes, R3 at three chunk sizes; latency, capture, index, amplification; variance beside every number.

G4. Part 2 table complete: both modes, every flow, bytes transferred against the census bound.

G5. Transport matrix complete for every non-stretch probe, null and file, memory and NVMe, with RoCE counters at zero.

G6. One command rebuilds the fleet from dated archives; one command reruns every table on a fresh pair.

## Descoping order

When the schedule slips, items come off from the top.

1. ibverbs daemon arm.
2. Super-chunk placement.
3. Mirror-on-FLUSH arm.
4. R2 dm-vdo.
5. Profile prefetch (depth prefetch stays).
6. Partitioned mode. Replicated mode alone still gives H2's transfer result.

Not removed under any slip: part 1, the nvmet TCP and RDMA probes, and the daemon over TCP.

## Risks

**Daemon overrun.** The largest risk and the reason G1 is at week 2. Protocol plumbing comes from maintained crates so the hours go to the components listed as new code on page 01.

**RoCE configuration.** GID selection, MTU, adaptive retransmission on a lossy fabric. Budgeted at 8 hours; if it exceeds 20, the RDMA rows are dropped and the TCP rows stand.

**Node availability.** 36 nodes of this type exist. Reserve the pair in week 1 for every measurement week.

**Known configuration pitfalls.** `dedup=on` means SHA-256; direct IO does nothing on zvols; the 100G interface stays down unless the profile declares a link on it.

**Census realism.** Scripted drift is not real drift. The fleet is built from real dated archives, the scripts are published, and the numbers it supplies are bounds the daemon is read against, not claims about fleets in the wild.

## Logistics

CS 4993, 1 credit.

Expectations in writing before Sep 9.

Thirty minutes of sponsor time every two weeks, with G1 as a scheduled meeting.

## Future work

**Availability.**
The mirror arm is the seed of replication before ack; with it and k ≥ 2 on N ≥ 3 the system has a failure model, which needs membership, failure detection, and rebalancing, none of which this study touches.

**Placement.**
Super-chunk placement for locality, and a cache policy that weighs a chunk's owner distance.

**The same split elsewhere.**
Prefix caching in LLM serving (vLLM, SGLang, Mooncake) names cached KV blocks by a hash chain over the whole token history, so two requests share only along a common prefix; that is lineage.
The same document after two different preambles is computed twice; that is the cross-host case here, and nobody has measured its size on a real trace.

---

# 06 Prior art

Swept on 2026-09-01; sources and what was actually opened are in `docs/review/`.

No prior system is a local-only write log with no network on the write path, a fleet-wide hash-placed chunk store, and remote cold reads under a stock hypervisor.

Three of them, Datrium, Nutanix, and Fossil with Venti, are close enough that a reviewer would cite them if they were omitted.

## Nearest systems

| Work | What it is | How this differs |
|---|---|---|
| Datrium DVX (2016), US20170031994A1 | host-side fingerprinting, host flash as read cache, global deduplication on a shared data-node pool; the patent lists host-only ack as an alternative | peers as owners by hash instead of a shared pool; open implementation on stock QEMU; the cold read measured per transport |
| Nutanix AOS | local OpLog on SSD, mirrored to another node before ack; cluster-wide post-process deduplication at 16K; per-node cache | no mirror on the write path, with the window measured and the mirror as an arm; placement by hash instead of by vDisk locality; numbers published |
| Fossil + Venti (2002) | a disk write buffer in front of a content-addressed archive; the two-tier shape | block device under a VM instead of a filesystem; primary capacity instead of archival; more than one owner |
| Ceph + TiDedup (ATC '23) | post-process CDC into a chunk pool placed by CRUSH on the fingerprint; promotes on a cold miss | writes never cross the network; a host cache instead of promotion; a guest block path; latency numbers, which TiDedup does not report |
| vSAN ESA global deduplication (2025) | cluster-wide post-process 4K deduplication, mirrored writes, 3 to 16 hosts, no published numbers | the per-host to cluster-wide change this study measures, with published numbers |
| HYDRAstor (FAST '09) | content-addressed blocks placed by DHT across a grid, global deduplication | secondary storage with network writes; no guest path |
| DeDe (ATC '09) | hosts hash in-band, deduplicate out-of-band against a shared index on a SAN, no coordinator | local disks instead of a SAN; chunks move to owners instead of pointers on shared storage |
| Liquid (TPDS '14) | fingerprint-keyed VM image filesystem, P2P fetch across hosts, copy-on-read local cache | block device under a stock hypervisor instead of a filesystem; owner by hash instead of P2P; full text not yet read |

## Remote fetch

| Work | What it measured | What it leaves open |
|---|---|---|
| DADI (ATC '20) | block-level lazy loading with tree P2P; 10,000 containers on 1,000 hosts in 4 s; trace prefetch removes 95% of the cold gap; reads from a parent's page cache are faster than local disk | no per-read miss latency; not content-addressed |
| Slacker (FAST '16) | only 6.4% of a container image is read at startup; lazy fetch over NFS; run phase 17% slower | no per-block miss cost; centralized |
| VMTorrent (CoNEXT '12), VMThunder (TPDS '14) | demand-priority P2P VM image streaming with recorded profiles | startup seconds only |
| FaaSnap (EuroSys '22), REAP (ASPLOS '21) | lazy page faults from local disk at 13 µs; userfaultfd over 128 µs uncached; working set 9% of footprint | memory, not disk; local |
| SnowFlock (EuroSys '09) | 275 µs per page fetched over gigabit, 82% of it in the network stack | the only in-VM remote per-unit number, and it is from 2009 |
| Dahlin et al. (OSDI '94) | cooperative caching: remote client memory at 1.25 ms against disk at 15 ms; N-chance forwarding | the argument this study repeats at 100 GbE with content-addressed chunks |
| CLB (VEE '17), Satori (ATC '09) | content-keyed sharing of VM disk reads across guests on one host; 95 to 98% of boot reads eliminated | single host; no store |

**Nobody has measured a content-addressed chunk fetched from a peer inside a VM block read path at microsecond scale.**

Every lazy-loading system reports startup seconds, admits a per-read penalty, and hides it with a recorded prefetch profile.

## Transport

i10 (NSDI '20) and blk-switch (OSDI '21) showed kernel TCP can match RDMA on throughput per core with batching, at a latency cost of 50 to 100 µs at low load.

The SPDK 24.05 reports on ConnectX-5 put kernel nvme-rdma at 12.1 µs and kernel nvme-tcp at 21.4 µs for a 4K read against a null device.

Homa (ATC '21) and eRPC (NSDI '19) put kernel bypass at 2 to 4 µs and attribute the rest of kernel TCP to wakeups and core selection.

No storage paper measured a blocking userspace daemon over kernel TCP as a remote read target; that row is estimated on page 04 and measured here.

## Objections already in print

**Dong et al. (FAST '11)** rejected per-chunk hash placement for backup streams on locality grounds and routed 1 MB super-chunks; page 03 answers with a local cache and page 04 measures the cost.

**Meyer and Bolosky (FAST '11)** already showed deduplication savings grow with the log of the number of machines in one domain, which is the capacity half of H2 stated for desktops.

**Jin and Miller (SYSTOR '09)** found fixed blocks match CDC on VM images, which is why part 1 predicts a tie.

**despairlabs (2024)** tells ZFS operators to use clones and block cloning for the copy case and deduplication rarely; the study agrees on one host and disagrees across hosts.

## What remains

Datrium's patent and Nutanix's design are cited by name.

Fossil and Venti are cited as the origin of the two-tier shape.

The study's contribution is the measurement: what content addressing provides across hosts on commodity hardware under a stock hypervisor, and what the remote cold read costs, per transport.
