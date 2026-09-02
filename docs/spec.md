# Content Addressed Deduplication: A distributed storage system study

CS 4993, fall 2026. Research spec, v8.

This file is generated from `playbook/src/routes/00–06` by `docs/mkspec.py` for review and hand edits.
Edits here are ported back into the pages, then the file is regenerated.
Figures are described in brackets where the pages draw them.

---

# 00 Thesis

A deduplication table shares duplicate data within one host.

Content addressing shares it across hosts.

On Linux VM fleets the first is already solved by ZFS.

So the case for content addressing rests on what a hash does that a block address cannot: move only unique bytes between hosts, store each chunk k times across a fleet instead of once per host, and serve a chunk from whichever host holds it in memory.

This study builds that system on a stock hypervisor and measures what each of those provides and what each costs.

## Current solutions: single-host, cross-VM deduplication

Consider two VMs that each run `apt upgrade` and download the same packages. Their disks now hold the same bytes. Under copy-on-write alone no clone can share them, because neither copy descends from the other.

ZFS solves this with a deduplication table (the DDT), and the Linux kernel has dm-vdo, which does block-level deduplication, compression, and thin provisioning under any filesystem.

Both hash every block and share equal ones at a fixed, aligned block size: 4K for dm-vdo, and the volblocksize for a ZFS zvol, 16K by default.

Nearly everything inside a Linux guest is 4K aligned (ext4 uses 4K blocks, partitions start at 1 MiB, and package managers write whole files), so at 4K a deduplication table reaches nearly all of the duplicate data.

This is why [Jin and Miller](https://ssrc.us/media/pubs/082a25b906aa716ca3c2439b8c1889449ecac44c.pdf) found in 2009 that fixed blocks match content-defined chunking on VM images.

**Therefore, on a single host, content addressing has zero capacity win over ZFS at a 4K chunk size.**

Part 1 of this study measures our CAS system against ZFS and tests this assertion.

None of this works across hosts.

The DDT is per pool, dm-vdo has no replication, and `zfs send` has not carried a deduplicated stream since OpenZFS 2.0.

With a fleet of N hosts each running its own deduplication store, every unique chunk is stored N times.

As a side effect, moving a VM to another host means sending every one of its chunks over the wire (via `zfs send` or similar), whether or not the destination already holds them.

Shared-storage systems (Ceph RBD with TiDedup, for example) do deduplicate across hosts, by putting every write on the network before it is acknowledged.

That is the other end of the design space, and page 06 places this study against it.

[figure: Left: two hosts each with their own deduplication table and no link between them; the same chunk is stored on both and moves whole when a guest migrates. Right: two hosts whose chunks are named by hash and owned by hash across both; a chunk is stored k times fleet-wide, a guest's manifest moves while its chunks stay, and a cold read fetches by hash from the owner.]

## Advantages of content-addressing your data

A chunk named by its hash has the same name on every host, so placement is a function of the content itself.

Three things follow, and each is a measured claim in parts 2 and 3.

**Transfer.**

Provisioning a guest moves only its manifest (32 bytes per chunk) and no chunk data, because every chunk it names already exists at its owner.

Migrating a guest moves the manifest plus whatever it wrote since the last compaction.

Migration is not the point of the study; it is the operation where "only unique bytes cross the wire" is easiest to see and to measure.

**Capacity.**

A fleet stores each chunk k times, not once per host.

**Cache.**

A chunk that is hot anywhere is in some host's memory, and **a peer's memory over 100 GbE is closer than local NVMe**: about 20 µs against about 80.

## The cost

Three costs come with any deduplicating store, and we measure them rather than assume them: write amplification (every byte is written to the staging log and again to the chunk store), compactor interference (compaction shares the guest's disk), and index memory (one entry per chunk, in RAM).

One cost is specific to crossing hosts: the network sits on the read path for cold chunks.

A guest read whose chunk lives on another host pays one round trip.

Part 3 measures that round trip over TCP and over RDMA (remote direct memory access), from the peer's memory and from the peer's NVMe.

It then measures prefetch: the daemon knows from the manifest which chunks come next, so it fetches them before the guest asks, and the round trip overlaps with work the guest is already doing instead of adding to it.

How much of the cost prefetch removes is the number.

One cost is a trade rather than a tax: durability before acknowledgment.

A guest's FLUSH means "these bytes must survive".

In local class, the default, the daemon acknowledges after fdatasync on this host, which is the same contract a local disk gives; if the host is lost before compaction has shipped those bytes to their owner, they are lost with it.

In fleet class the daemon first sends the bytes themselves (not the manifest, since the manifest points at bytes that exist nowhere else yet) to a fixed peer, waits for the peer's fdatasync, and then acknowledges; the bytes now survive the loss of this host.

Every hyperconverged product works in fleet class.

Parts 2 and 3 measure the price of the difference: one round trip and one remote fdatasync per FLUSH, on TCP, and on RDMA if that arm lands.

## Hypotheses

**1. Single-host parity.**

Our CAS system stores within 10% of the bytes ZFS fast dedup stores at the same block size, with guest p99 within 20% of a raw file on XFS.

Index bytes per TB fall in inverse proportion to chunk size.

**2. Multi-host benefits.**

Provisioning and migrating a guest between hosts move the manifest plus the uncompacted tail, within 10% of that bound.

With one copy per chunk, the two-host testbed stores at most 55% of what two per-host deduplication stores hold.

Two hosts is the floor of this gain: Meyer and Bolosky showed deduplication savings grow with the log of the number of machines in one domain, so a larger fleet gains more, not less.

**3. The cost of a read over the network.**

A chunk served from the owner's memory arrives faster than a local NVMe read on both TCP and RDMA.

From the owner's NVMe it costs at most 40% over local on TCP and 15% on RDMA.

With enough reads in flight, remote sequential throughput matches local.

**4. The cost of durability before acknowledgment.**

Fleet class costs one round trip and one peer fdatasync per FLUSH.

Its write p99 at QD1 is within 3x of local class on TCP, and within 2x on RDMA if the ibverbs arm lands.

In local class, a lost host loses exactly the acknowledged bytes not yet compacted to an owner, and that window is reported in seconds.

Thresholds come from the transport literature on page 04 and the census prediction on page 02.

They are frozen at the end of week 2 and do not move.

## Outputs

**The system.**

A content-addressed block backend for VMs under unmodified QEMU on a stock Linux kernel, over kernel TCP, with source, configuration, and the scripts that produce every table.

**The single-host table.**

Our CAS system against ZFS fast dedup and a raw file on XFS: bytes stored, guest p99, write amplification, and index memory, at three chunk sizes.

This is where hypothesis 1 is decided and where the chunk-size trade-off is measured.

**The multi-host table.**

Bytes moved to provision and to migrate a guest, bytes sent to synchronize two drifted guests, and fleet bytes stored with one copy per chunk, each against what `zfs send` or rsync would move and what two per-host ZFS pools would hold.

No local-disk backend can produce these numbers.

**The remote-read measurement.**

A content-addressed chunk fetched from a peer under a VM block device, at microsecond resolution, over the daemon on kernel TCP and over NVMe-oF on TCP and RDMA, from the peer's memory and from its NVMe, with and without prefetch.

No published system has this measurement.

**The durability trade.**

Local class against fleet class on the same hardware: the write latency fleet class costs per transport, and the seconds of acknowledged data local class puts at risk.

## Scope

The study covers hosts that serve guests from local flash, from a homelab up to rack scale.

Storage arrays and hyperscale economics are out of scope.

Chunks are placed over N hosts by rendezvous hashing: every host scores each (chunk, host) pair with one hash function, and the k highest-scoring hosts own the chunk.

Every host computes the same answer with no shared state, no ring, and no lookup, which is what makes it simpler than a consistent-hashing ring at small N.

The testbed is two hosts with static membership, so failure detection, rebalancing, and authentication are out of scope.

One copy per chunk (k = 1) on two hosts is a measurement configuration that maximizes remote reads so their cost can be seen; a deployment runs k ≥ 2 on N ≥ 3 hosts.

Each image has one writer.

The study migrates disks only; memory migration is QEMU's.

The guest contract is virtio-blk with a volatile write cache: an acknowledged FLUSH is durable and nothing else is.

Every configuration runs with QEMU `cache=none`, so the host page cache is bypassed everywhere.

Equal BLAKE3 hashes are taken to mean equal bytes; a sample of matches is verified byte for byte and the sample size is reported.

The store is trusted infrastructure, so deduplication side channels are documented and excluded.

Experiments run at single-digit TB, and larger figures are formulas with measured constants, labeled as such.

RDMA is a measurement arm on page 04; nothing in the architecture requires it.

---

# 01 Architecture

The network is on the read path only, only for cold chunks, and never on the write or flush path.

Every design choice on this page follows from that invariant.

## Components on one host

The guest sees a virtio-blk device on stock QEMU.

QEMU connects it over vhost-user-blk to one process per host, the daemon, and all new code lives there.

Guest memory is shared with the daemon, so requests are read in place, and storage IO goes through io_uring.

[figure: The per-host datapath. A guest on stock QEMU reaches the daemon over vhost-user. Writes append to a local staging log and are acknowledged at FLUSH after fdatasync. A background compactor chunks settled extents, hashes them, and either appends unique chunks to the local store or sends them to their owner on another host, waiting for a durable ack. Reads check staging, then the local store, then the chunk cache, then fetch by hash from the owner.]

## Write path

Guest writes append at block granularity to a staging log on local NVMe.

Every append is stamped with a per-image sequence number inside the same critical section as the append, so replay preserves last-write-wins.

FLUSH is `fdatasync` of the log, then the acknowledgment, and it covers the highest sequence number seen on any queue of the device, because virtio-blk has no FUA (force unit access) and requests arrive on several queues.

The hot path hashes nothing and chunks nothing, so large writes proceed at sequential-append speed.

Durability comes from the log alone: the page cache never holds the only copy of anything, and every file is opened O_DIRECT.

The log is flushed the moment a FLUSH is waiting; there is no linger window, because a linger against a 40 µs fdatasync is slower than the sync.

Staging is finite, so a governor paces compaction on the measured drain rate, with an idle trigger so nothing sits parked after a workload ends.

When ingest still outruns compaction the guest sees added latency, never a stall, and the log ends in a clean ENOSPC.

The point where pressure engages, and the latency it adds, are both measured.

## Compactor

A background pass reads settled extents from staging, cuts them into chunks, hashes each with BLAKE3, and skips any hash that every current owner already holds and has fenced; a copy in a cache does not count.

Chunking is fixed 4K or FastCDC with boundaries snapped to 4K, chosen per arm on page 02.

Settled means unwritten for a settle window, so an extent overwritten inside the window is chunked once, in its final form; the window is a parameter and its effect on chunk traffic is measured.

Deferred hashing behind a write buffer is Liquid's design (TPDS '14) and Fossil's before it; the difference here is that the buffer is a durable log with a FLUSH contract rather than volatile memory flushed at shutdown.

For each new chunk, the owner is the first k hosts in rendezvous order of its hash.

If the owner is this host, the chunk is appended to the local store and written with fdatasync.

Otherwise it goes to the owner in a sealed segment of many chunks, which the owner appends, fdatasyncs once, and acks.

**Only after the ack does the extent count as compacted.**

A chunk the compactor has produced stays pinned, in staging or in the store, until the manifest commit that references it is durable, and an owner never reclaims a chunk it acked before that fence.

Staging is therefore the write-ahead log for the whole fleet.

Two costs come with this design, and both are measured.

Every surviving byte is written at least twice, staging then store, plus journal traffic.

Compaction reads and writes the same device the guest is using, so guest p99 is measured with the compactor active and idle.

CDC over a dirty extent re-chunks from the last settled boundary before it to the first boundary after it that agrees with the existing cut.

This is the standard resynchronization rule ([LBFS](https://pdos.csail.mit.edu/papers/lbfs:sosp01/lbfs.pdf) locality; [Xet](https://huggingface.co/docs/xet/en/chunking)'s boundary reset), and it is why CDC never runs on the hot path: one aligned write can move every boundary in its neighborhood.

## Read path

Reads check staging, then the local store, then the chunk cache, then send `GET(hash)` to the owner.

The owner answers from its cache if the chunk is hot, otherwise from its store.

Fresh data is served without indirection; settled data incurs the manifest lookup, the index lookup, and, if the owner is remote, one round trip.

`GET` runs on its own connections with priority over `PUT` and over compaction IO at the serving disk, so a guest-blocking read never waits behind a bulk transfer.

The chunk cache is daemon-owned memory keyed by hash, LRU, with a size that is a parameter.

Fetched chunks that this host does not own live in that memory cache only. Liquid persisted them in an on-disk copy-on-read cache; here a refetch from a peer's memory costs about 20 µs while a local disk hit costs about 80, so the disk tier pays only for chunks that are cold at their owner too. It is a knob, noted, and measured only if time remains.

Because every file is O_DIRECT, the kernel page cache holds nothing on any host, and the cache size is set equal to ARC on the ZFS configuration.

Prefetch is the daemon issuing the next D hashes from the manifest when it sees sequential reads, and optionally replaying a recorded boot profile.

D is swept on page 04.

## Store, index, and manifest

The local store is an append-only log of records (length, hash, checksum, bytes) and is authoritative for the chunks this host owns.

The index maps hash to offset, lives in memory, and is rebuilt by scanning the store without re-hashing, because the hash is inline; its bytes per TB is the constant the chunk-size arms measure.

The index is written only after the data it points to is durable, at every fence.

The manifest, one per image, is a journaled tree from disk offset to chunk hash.

It lives with the guest's host and moves when the guest does.

## Protocol

| Message | Reply | Used by |
|---|---|---|
| GET(hash) | bytes | cold read, prefetch |
| PUT(segment) | ack after one fdatasync | compactor sending a sealed segment of chunks to an owner |
| HAS(hashes) | bitmap of hashes the owner lacks or has not fenced | compactor before PUT, so only missing chunks are sent; provisioning verification |
| LIVE(epoch, hashes) | ack | garbage collection |
| JOURNAL(image, range) | ack after fdatasync | fleet class: the staging tail to the fixed journal peer on FLUSH |

Messages are length-prefixed over kernel TCP with `TCP_NODELAY`, driven by io_uring.

`GET` and `JOURNAL` have their own connections and priority; `PUT` is bulk.

Every message is idempotent and named by hash or sequence number, so any of them can be retried.

The daemon runs busy-polling or blocking; page 04 measures both, because the scheduler wakeup is part of the cost.

Rendezvous hashing means a reader already knows the owner of every hash, so nobody looks up anyone else's index.

RDMA and NVMe-oF exports appear on page 04 as probes that show what the kernel stack costs; the architecture does not depend on either.

## Placement and the parameter k

The owner set of a chunk is the first k hosts in rendezvous order of its hash.

The journal peer for fleet class is not chosen this way: a journal needs a fixed home with ordered replay, so each image names one peer at creation and keeps it.

k is the one multi-host parameter.

With N hosts, k = N places every chunk on every host (replicated) and k = 1 places each chunk on exactly one (partitioned); on the two-host testbed these are k = 2 and k = 1.

Page 03 measures both, and a deployment would run k ≥ 2 on N ≥ 3 hosts.

## Durability classes

Durability is a per-image class on one pipeline; the class changes who waits at FLUSH and for how long, and nothing about where bytes end up.

**Local class**, the default: FLUSH returns after fdatasync of the staging log on this host.

**Fleet class**: the staging tail since the last FLUSH is sent to the image's journal peer, which appends it to its own log and fdatasyncs; FLUSH returns after both.

Local class is the contract a local disk gives, which is why it is the default against R0 and R1.

Fleet class is what every hyperconverged product does before it acknowledges, and page 03 measures what it costs.

| Failure | Local class | Fleet class |
|---|---|---|
| daemon crash | everything: replay the staging log from D, re-run compaction | same |
| host crash, power loss | everything acked: FLUSH was fdatasync on local NVMe | same |
| host lost | acknowledged bytes not yet compacted to an owner, exactly (D, E], are lost; R0 and R1 lose everything | the staging tail survives: the journal peer replays (D, E] onto a new host; chunks the lost host owned survive only if k ≥ 2, as in the row below |
| peer lost, k = 1 | chunks it owned are unreadable until it returns, and lost if its disk is; a read that needs one waits or fails with an error, never returns stale bytes |

Two rules hold in both classes.

Bytes are durable on local NVMe before they go on the wire, always.

Transfer is two-phase: the owner fdatasyncs and acks before the sender marks anything compacted or reclaimable.

## The watermark

Every image carries two integers.

E is the highest sequence number with no unconfirmed append before it; in local class confirmed means on local NVMe, in fleet class it means on the journal peer too.

D is the highest sequence number whose chunks are durable at their owners and whose manifest entries are committed.

FLUSH waits for E. A snapshot cuts at E. The staging log is trimmed below D. Recovery and migration replay exactly (D, E].

E never skips a hole, because a maximum over confirmations is the answer that loses acknowledged data.

Two logs, staging and the manifest journal, must agree after a crash, and staging is senior.

Compaction is idempotent, so replaying (D, E] and re-running it produces the same chunks and the same manifest.

`kill -9` at any point, then this replay, must pass `fio --verify` before any number from the daemon is reported.

Three more cases have tests because each has stalled a guest in a production system: a FLUSH racing writes on another queue, a discard of an unwritten range, and a daemon that stops answering, which leaves the guest in D-state forever because virtio-blk has no timeout.

## Garbage collection

A chunk is live if any manifest on any host references it, or if an in-flight compaction has pinned it.

Each host sends its owner the live set for an epoch with `LIVE`, and the owner sweeps with `FALLOC_FL_PUNCH_HOLE` over dead records; there are no reference counts. Liquid ran the same mark-and-sweep with Bloom-filter live sets over its data servers.

ZFS frees an overwritten block the moment its reference count drops; this design does not, so space leaks between sweeps.

The sweep therefore runs before every capacity measurement, and the bytes it reclaims are reported beside the capacity number as the leak; concurrent collection is out of scope.

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
| host filesystem | XFS on the dedicated NVMe, O_DIRECT, hole punching; ZFS never sits under the daemon |  |
| staging, watermark, governor, compactor, store, index, manifests, cache, protocol, journal peer, garbage collection | this study | new code |

Because the hypervisor is unmodified, no result can be an artifact of a patched QEMU, and the raw-file control runs the identical binary.

---

# 02 One host

Part 1 runs the same stock QEMU, the same guest, and the same NVMe device, and varies only the storage behind the device.

The prediction is a tie on capture between our CAS system and ZFS fast dedup.

It is measured anyway, because the chunk-size curve under it is the single-host design result, and because the comparison against ZFS is the first objection a reviewer will raise.

## Configurations

**R0. Raw file on XFS.**

QEMU's raw driver on the dedicated NVMe.

The control, with no deduplication anywhere in the path.

**R1. Zvol on ZFS 2.3 fast dedup.**

Own pool on the same device, created and destroyed per run, opened by QEMU as a block device.

`feature@fast_dedup`; `dedup=blake3`, since `dedup=on` silently uses SHA-256 regardless of the checksum property; `volblocksize=16K` primary and `4K` second arm; `compression=zle` outside the compression arm so zero blocks do not collapse onto one DDT entry; `dedup_table_quota` unset and `zpool ddtprune` never run during a measurement; DDT memory from `zpool status -D`.

OpenZFS direct IO does not apply to zvols or with deduplication enabled, so R1 is ARC-backed in every arm, and the paper reports it as such.

**R2. Raw file on XFS over dm-vdo** (optional).

Inline fixed-4K deduplication in the kernel, mainline since 6.9, with its own XFS instance on the vdo device.

Index memory from `vdostats`.

**R3. Our CAS system on one host.**

Local store only, so k does not apply.

Three chunk-size arms, below.

R0 against R3 is the cost of the daemon with everything else held constant.

R1 is the deployed state of the art and differs in kernel boundary, caching, and allocation, so it is a case study beside the controlled pair, and the paper attributes deltas accordingly.

## Chunk-size arms

Fixed 4K captures everything a Linux guest offers, and costs an index entry per 4K: about 250 million entries per TB, roughly 10 GB of memory per TB at 40 bytes each.

That is the DDT memory cost the daemon is designed to avoid.

FastCDC at a 16K mean cuts the index four times over and loses some aligned matches.

The one prior curve on VM images is Liquid's: 77% deduplicated at 4 KB falling to 59% at 256 KB, with 256 KB chosen for HDD seek cost; on NVMe the seek term is gone and the trade is index memory alone.

Three arms: fixed 4K, fixed 16K, FastCDC 8K to 64K with a 16K mean.

CDC boundaries snap to 4K, so no guest block straddles two chunks and a 4K overwrite invalidates one chunk, not two.

Reported per arm: bytes stored, index bytes per TB, guest p99, write amplification.

**Capture against index memory as a function of chunk size is the result this page produces.**

The census below predicts the capture column before any run.

## Workloads

- fio: 4K random write and read at QD1 and QD32; 128K sequential.
- Boot storm: N clones of one image booted together, N = 4, 16, 32.
- Fleet replay: the synthetic fleet below written onto N guests, at two points on its timeline.
- Overwrite: a small SQLite database rewriting its pages in place for an hour, with guest discard on. This is the case where a store without reference counts leaks between sweeps and ZFS does not.

There is no kernel build and no synthetic stress workload that exists only to exercise the daemon.

## Metrics

- Guest p50 and p99 write and read latency against R0, compactor active and idle. Reported first.
- Bytes stored after compaction completes and the sweep has run, against the census prediction at the configuration's block size; bytes the sweep reclaimed reported beside it as the leak.
- Index or DDT bytes per stored TB.
- Write amplification: device bytes written per guest byte, from NVMe counters, with both legs (staging and store) reported, not one.
- Sustainable ingest, the point where the governor starts adding latency, and how much it adds.
- Chunk traffic against the settle window: chunks produced per guest byte written, on the overwrite workload.
- Compactor CPU per GB ingested, per chunk-size arm; hashing cost was Liquid's stated reason for large blocks and is a number here, not a reason.
- Recovery: `kill -9`, replay, `fio --verify`; FLUSH racing writes on another queue; discard of an unwritten range; a daemon that stops answering.

## Controls

Pinned vCPUs, performance governor, discarded warm-up, fresh filesystem or pool per repetition, at least five repetitions, variance beside every number.

With `cache=none`, R0 and R2 have no host cache; `zfs_arc_max` on R1 and the daemon's cache size on R3 are set equal.

All configurations are observed at the guest boundary (fio's histograms, guest-side blktrace for the boot storm) plus host device counters.

The daemon adds per-request stage timestamps drained to ndjson, cross-checked once against bpftrace with the delta reported.

`zpool` and `vdostats` figures are supplementary.

## Prediction from a small census

A small census supplies two numbers the rest of the study is measured against: how many unique bytes a fleet holds at a given block size, and how many bytes copy-on-write would already have shared.

**Phase 0.**

`zdb -S` on a ZFS pool holding the cloned fleet.

Pool traversal starts each dataset at its previous snapshot's txg, so blocks a clone inherited from its origin are counted once, and the simulated ratio is duplicates beyond what clones already share.

Verified in `dmu_traverse.c`, and confirmed by a five-minute test before it is cited.

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

Part 2 runs the same CAS system on N hosts with one parameter, k.

Every number on this page is one no local-disk backend can produce.

## Two placement modes

k is the number of owners per chunk.

The design supports any N; the testbed has two hosts, so k takes two values, and they are two different experiments.

With k = 1, a host that goes down takes its chunks with it until it returns; a read that needs one waits or fails with an error, and nothing is lost if the disk comes back.

Surviving a host dark at two hosts costs a full mirror of chunks (k = 2) plus fleet class for the staging tail.

[figure: Left, replicated: k equals 2, every chunk is on both hosts, compaction sends each new unique chunk once, and no read is ever remote. Right, partitioned: k equals 1, each chunk lives on the host its hash selects, fleet capacity is one copy per chunk, and about half of a guest's cold reads go to the other host.]

## Provisioning

A new guest on host B from an image whose chunks exist anywhere: copy the manifest, at least 32 bytes per chunk, about 80 MB for a 40 GB image at 16K chunks. Every chunk it names already exists at its owner.

In replicated mode no other data is transferred.

In partitioned mode no other data is transferred either, because chunks are fetched on first read.

**Provisioning cost is the size of the manifest.**

Baseline: `qemu-img convert` or `scp` of the raw file, and `zfs send | zfs recv` of the zvol, each moving the allocated size of the image.

Liquid measured this comparison in 2014 on 1 GbE (8 GB to seven nodes: 730 s by scp, 35 s by Liquid) in seconds; here it is bytes on the wire at 100 GbE.

## Migration

To move a guest from A to B, the daemon freezes the device on A and takes E, hands the image to B by one fenced swap of its root record (which names the writer), ships the manifest and the staging extents in (D, E], and resumes on B.

A accepts no write after the swap, and B resumes only after the swap names it.

On resume the log is reconciled by evidence, the local high-water against the durable head, never by who claims to own it.

A 40 GB guest that compacted recently moves in tens of MB.

Bytes are the small part of a migration: the disk cut is milliseconds and the rest of the blackout is orchestration, so the blackout is reported decomposed into freeze, swap, transfer, and resume, beside the bytes.

Memory migration is QEMU's and is out of scope; this is the disk.

Baseline: rsync of the raw file, and `zfs send` of the zvol.

Since 2.0 `zfs send` emits no deduplicated stream, so the bytes are the logical size regardless of the DDT.

## Synchronization after drift

Two guests, one on each host, are cloned from the same image and each updated independently to the same package set.

Compaction on each host sends only the chunks the owner lacks, packed in sealed segments.

Bytes on the wire are read against the census's unique-byte count for the pair.

Chunks per second is reported beside bytes per second, because per-chunk cost is what caps a replication path before the link does.

This is the `apt upgrade` case from page 00, measured.

## Capacity

Partitioned mode stores each chunk once across the fleet.

Measured: bytes on both stores after the fleet replay completes and the sweep has run, against two per-host ZFS pools holding the same guests.

Predicted: about half.

Also measured: what fraction of a guest's cold reads went to the other host, which on two hosts with k = 1 should be about half and is the worst case any fleet would see.

## Durability classes and their cost

In local class, between a FLUSH ack and the chunk being durable on its owner sits the compaction lag, (D, E] in the watermark's terms.

It is reported in seconds under the fleet replay, as a distribution, with the segment size as the parameter.

That window is what a lost host loses, and it is the RPO (recovery point objective) of local class.

In fleet class, the staging tail goes to the image's journal peer on every FLUSH and the ack waits for the peer's fdatasync, which is what every production system in this space does.

The class costs one round trip plus one remote fdatasync per FLUSH, and it is measured as write p99 at QD1 against local class, on TCP, and on RDMA if the ibverbs arm lands.

This is the one place where the transport is a large share of the cost rather than a tenth of it.

## Measurements

| Flow | Daemon | Baseline | Read against |
|---|---|---|---|
| provision | bytes transferred, both modes | scp of raw file; zfs send | manifest size |
| migrate | bytes transferred, both modes; blackout decomposed | rsync; zfs send | manifest size + staging tail; milliseconds for the cut |
| sync after drift | bytes and chunks per second sent by compaction | rsync; zfs send | census unique bytes |
| capacity | bytes stored, partitioned, after the sweep | two per-host ZFS pools | census prediction |
| remote fraction | cold reads served by the peer |  | about half, worst case |
| local-class window | seconds from ack to owner-durable |  | the RPO of local class |
| fleet-class cost | write p99 at QD1, TCP and RDMA | local class | one RTT plus one remote fdatasync per FLUSH |

## The locality objection

[Dong et al. (FAST '11)](https://www.usenix.org/legacy/events/fast11/tech/full_papers/Dong.pdf) rejected per-chunk hash placement for backup because it destroys read locality, and routed 1 MB super-chunks instead.

This is primary storage with a local cache, so the fragmentation cost they argued about is measured directly on page 04.

If it is large, placement by super-chunk is the knob, noted here and measured only if time remains.

---

# 04 Remote read

Part 3 measures the one place the network enters guest latency: a cold read whose chunk lives on another host.

This page measures that read and then reduces it.

## Where the time goes in a remote read

Every read has about 80 µs of NVMe media time under it.

On 100 GbE the transport sits on top: raw RDMA adds 3 to 5 µs, kernel nvme-rdma about 12, kernel nvme-tcp about 21, and a userspace daemon over kernel TCP 20 to 30.

Those are the [SPDK 24.05](https://review.spdk.io/download/performance-reports/SPDK_rdma_mlx_perf_report_2405.pdf) and [Systor '17](https://www.systor.org/2017/slides/NVMe-over-Fabrics_Performance_Characterization.pdf) numbers on ConnectX-5, and the testbed replaces them.

RDMA against TCP is therefore a 10 µs difference on an 80 µs read.

The larger factor, about 4x, is whether the chunk is in the owner's memory or on its disk.

If those numbers hold, **a chunk from a peer's memory over TCP arrives faster than one from local NVMe**, and hypothesis 3 tests this.

With hash placement, a chunk shared across the fleet is hot at exactly one owner, so every host's read of it hits that owner's cache.

[figure: Horizontal bars, one per case, length proportional to latency from the literature. Local NVMe about 80 microseconds. Peer RAM over RDMA about 12, over TCP about 21, over the daemon on TCP 20 to 30. Peer NVMe over RDMA about 92, over TCP about 101, over the daemon about 110. The peer memory bars are all shorter than the local NVMe bar.]

## Transport probes

The architecture's transport is the daemon over kernel TCP.

The other rows exist to show what the kernel stack and the userspace hop each cost, and nothing depends on them.

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
- Two load states for the file rows: quiet, and with `PUT` traffic running on its own connection at the ship rate from page 03, because a cold read in deployment competes with compaction. The difference is what the read-priority rule on page 01 buys.
- 4K, 16K, 64K. p50, p99, p99.9. Five runs of 30 s, caches dropped between, medians with spread.
- QD sweep 1, 4, 16, 64 for throughput and CPU per IOPS on both ends; TCP costs about 2.5x the CPU of RDMA at equal IOPS and the paper shows the ratio it measures.
- RoCE hardware counters (`out_of_sequence`, `packet_seq_err`, `local_ack_timeout_err`) printed beside every RDMA number, proving zero retransmits on a fabric with no PFC.

## Prefetch

The manifest tells the daemon what comes next.

Depth sweep: sequential reads through the manifest with 1, 2, 4, 8, 16, 32 chunks in flight, at 4K and 64K.

The bandwidth-delay point is about 250 KB for the fabric and about 1 MB with media under it, so roughly 20 chunks of 64K or 300 of 4K outstanding should hide the remote entirely.

Success is remote sequential throughput within the error bars of local.

Profile prefetch: record the chunk sequence of one boot, replay it on later boots.

Every lazy-loading system that has published numbers does this and reports it removing most of the miss cost; [DADI](https://www.usenix.org/system/files/atc20-li-huiba.pdf) says 95%.

It is the consensus mitigation and a one-day implementation.

## Under a guest workload

Partitioned boot storm at N = 16, with and without profile prefetch, against the same storm in replicated mode.

Reported: guest p99 and host device reads per guest byte.

**The gap between partitioned with prefetch and replicated is the residual cost of one copy per chunk.**

## The FLUSH round trip

Fleet class on page 03 puts one round trip and one remote fdatasync in front of every FLUSH acknowledgment, and there is no 80 µs of media to hide behind, so the round trip and the peer's fdatasync are the whole cost.

It is measured here with the same discipline as the read rows: write p99 at QD1 for local class, for fleet class over the daemon on TCP, and for fleet class over ibverbs if that arm lands, with the peer's fdatasync time reported separately so the transport's share is visible.

## RDMA on this testbed

The CloudLab fabric is lossy: no PFC or ECN is documented on the shared switches, and published work on this node type ran RoCE that way.

Adaptive retransmission is enabled on the NIC and the counters above prove the runs were clean.

ConnectX-5 cannot do io_uring zero-copy receive, so that option is unavailable.

None of this touches the architecture, which runs on kernel TCP and would run on any Ethernet.

## Hypothesis 3, restated

- A chunk from the owner's memory arrives faster than a local NVMe read, on TCP and on RDMA.
- From the owner's NVMe it costs at most 40% over local on TCP and 15% on RDMA, at QD1, 4K.
- At depth at or above the bandwidth-delay point, remote sequential throughput is within 10% of local.
- Partitioned boot storm p99 with profile prefetch is within 25% of replicated.

---

# 05 Plan

Fourteen weeks at about 320 hours, which is 23 a week against the 8 the course credit corresponds to.

The plan is sized to the work, and the descoping order defines what is removed if it slips.

## Hardware

Two CloudLab c6525-100g nodes (Utah), reserved as a pair.

Per node: AMD EPYC 7402P, 24 cores at 2.80 GHz; 128 GB ECC DDR4-3200; two 1.6 TB PCIe 4.0 NVMe SSDs; ConnectX-5 Ex 100 GbE, one port on the experiment network.

One NVMe holds the system and results, and the other is the device under test.

The pair is one hop through a single switch.

RoCE between two of these nodes works and has been used in published work on this exact hardware, on a lossy fabric.

Self-built kernels are routine there; the Ubuntu 24.04 image ships 6.8, dm-vdo needs 6.9, and OpenZFS 2.3 is a source build, so a kernel and ZFS are built once in week 1 and snapshotted as an image.

Reservations expire at 16 hours by default, so every run is scripted to complete inside one.

CloudLab is free for research.

A project is opened by a faculty member and reviewed by CloudLab staff, so the sponsor opens it before Sep 9.

Fallback: two OVHcloud Advance-4 2026 servers (EPYC 4585PX, 16 cores, 64 GB DDR5 ECC, 2 × 960 GB NVMe) on a 25 Gbps private link, which loses the RDMA arm and replaces the 100 GbE fabric with 25 GbE.

## Schedule

| Weeks | Build | Measure |
|---|---|---|
| 1–2 | vhost-user-blk daemon in passthrough: staging log, FLUSH, replay. Kernel and ZFS image. | R0; passthrough within 10% of R0 p99 (G1). Thresholds frozen. `zdb -S` phase 0 on the synthetic fleet. |
| 3–5 | Compactor with settle window, store, index, manifests, watermark, governor, recovery. Three chunk-size arms. | `kill -9` recovery and the three ordering tests pass (G2). First capture numbers. |
| 6–7 | R1 configured, both volblocksize arms. R2 if time permits. | Part 1 table complete (G3), sweep before every capacity number. |
| 8–9 | Protocol with separate GET and PUT connections, rendezvous placement, k, segment PUT with durable ack, HAS, pins, sweep. Provisioning; migration with the fenced handoff. | Replicated mode on two nodes. |
| 10 | Partitioned mode. Fleet class over TCP. | Part 2 table complete (G4). |
| 11–12 | nvmet exports, RoCE configuration, busy-polling and blocking daemon, depth prefetch, profile prefetch. | Transport matrix and prefetch sweeps (G5). Partitioned boot storm. |
| 13–14 |  | Report; reproducibility pack (G6). |

## Gates

**G1.** Passthrough daemon under stock QEMU within 10% of R0 p99 by the end of week 2. If this slips, everything after it slips, and the sponsor is informed that week.

**G2.** `kill -9` at arbitrary points, replay, `fio --verify` passes, before any daemon number is reported. Three ordering tests pass with it: a FLUSH racing writes on another queue, a discard of an unwritten range, and a stalled daemon that is restarted with the guest still recoverable.

**G3.** Part 1 table complete: R0, R1 at two block sizes, R3 at three chunk sizes; latency, capture, index, amplification; variance beside every number.

**G4.** Part 2 table complete: both modes, every flow, bytes transferred against the census bound.

**G5.** Transport matrix complete for every non-stretch probe, null and file, memory and NVMe, with RoCE counters at zero.

**G6.** One command rebuilds the fleet from dated archives, and one command reruns every table on a fresh pair.

## Descoping order

When the schedule slips, items come off from the top.

1. ibverbs daemon arm, and with it fleet class over RDMA.
2. Super-chunk placement.
3. R2 dm-vdo.
4. Profile prefetch (depth prefetch stays).
5. Fleet class over TCP. Hypothesis 4 is then reported as untested, with the literature's numbers as the estimate.
6. Partitioned mode. Replicated mode alone still gives hypothesis 2's transfer result.

Not removed under any slip: part 1, the nvmet TCP and RDMA probes, and the daemon over TCP.

## Risks

- **Daemon overrun.** The largest risk and the reason G1 is at week 2. Protocol plumbing comes from maintained crates, so the hours go to the components listed as new code on page 01.
- **RoCE configuration.** GID selection, MTU, adaptive retransmission on a lossy fabric. Budgeted at 8 hours; if it exceeds 20, the RDMA rows are dropped and the TCP rows stand.
- **Node availability.** 36 nodes of this type exist, so the pair is reserved in week 1 for every measurement week.
- **Correctness debt.** The bugs that stall a guest are known in advance: a FLUSH that misses a write on another queue, a discard that acknowledges a sequence number nothing wrote, a daemon that stops and leaves the guest in D-state. Each has a test in G2 and hours in weeks 3 to 5, before any number is taken.
- **Known configuration pitfalls.** `dedup=on` means SHA-256; direct IO does nothing on zvols; the 100G interface stays down unless the profile declares a link on it.
- **Census realism.** Scripted drift is not real drift. The fleet is built from real dated archives, the scripts are published, and the numbers it supplies are bounds the daemon is read against, not claims about fleets in the wild.

## Logistics

CS 4993, 1 credit.

Expectations in writing before Sep 9.

Thirty minutes of sponsor time every two weeks, with G1 as a scheduled meeting.

## Future work

**Availability.**

Fleet class is the seed of replication before ack; with it and k ≥ 2 on N ≥ 3 the system has a failure model, which needs membership, failure detection, and rebalancing, none of which this study touches.

**Placement.**

Super-chunk placement for locality, and a cache policy that weighs a chunk's owner distance.

**The same split elsewhere.**

Prefix caching in LLM serving (vLLM, SGLang, Mooncake) names cached KV blocks by a hash chain over the whole token history, so two requests share only along a common prefix; that is lineage.

The same document after two different preambles is computed twice; that is the multi-host case here, and nobody has measured its size on a real trace.

---

# 06 Prior art

Swept on 2026-09-01; sources and what was actually opened are in `docs/review/`.

No prior system combines a durable, sequence-numbered local write log with a stated FLUSH contract, a fleet-wide chunk store whose owners are the hosts themselves, a block device under a stock hypervisor, and a per-transport measurement of the remote cold read.

Liquid came closest in 2014 and is the row to read first; Datrium, Nutanix, and Fossil with Venti are close enough that a reviewer would cite them if they were omitted.

## Nearest systems

| Work | What it is | How this differs |
|---|---|---|
| Datrium DVX (2016), US20170031994A1 | host-side fingerprinting, host flash as read cache, global deduplication on a shared data-node pool; the patent lists host-only ack as an alternative | peers as owners by hash instead of a shared pool; open implementation on stock QEMU; the cold read measured per transport |
| Nutanix AOS | local OpLog on SSD, mirrored to another node before ack; cluster-wide post-process deduplication at 16K; per-node cache | no mirror on the write path by default, with the window measured and the mirror as fleet class; placement by hash instead of by vDisk locality; numbers published |
| Fossil + Venti (2002) | a disk write buffer in front of a content-addressed archive; the two-tier shape | block device under a VM instead of a filesystem; primary capacity instead of archival; more than one owner |
| Ceph + TiDedup (ATC '23) | post-process CDC into a chunk pool placed by CRUSH on the fingerprint; promotes on a cold miss | writes never cross the network; a host cache instead of promotion; a guest block path; latency numbers, which TiDedup does not report |
| vSAN ESA global deduplication (2025) | cluster-wide post-process 4K deduplication, mirrored writes, 3 to 16 hosts, no published numbers | the per-host to cluster-wide change this study measures, with published numbers |
| HYDRAstor (FAST '09) | content-addressed blocks placed by DHT across a grid, global deduplication | secondary storage with network writes; no guest path |
| DeDe (ATC '09) | hosts hash in-band, deduplicate out-of-band against a shared index on a SAN, no coordinator | local disks instead of a SAN; chunks move to owners instead of pointers on shared storage |
| [Liquid (TPDS '14)](https://madsys.cs.tsinghua.edu.cn/publications/TPDS2014-zhao.pdf) | FUSE file under a stock hypervisor; fixed 256 KB to 1 MB blocks hashed on flush or eviction from a 256 MB volatile write cache, pushed to range-partitioned data servers at VM shutdown; central meta server with refcounts; P2P Bloom-filter cache tier; copy-on-read disk cache; two replicas | a durable log with a FLUSH contract instead of a volatile buffer with no crash story; a vhost-user block device instead of FUSE; hosts as owners by rendezvous instead of a meta server and a data-server tier; exact HAS instead of Bloom filters; the miss cost measured, which Liquid names ("several times longer") and never measures |

## Remote fetch in prior systems

| Work | What it measured | What it leaves open |
|---|---|---|
| Liquid (TPDS '14) | 8 GB image to 7 nodes on 1 GbE: scp 730 s, NFS 510 s, BitTorrent 95 s, Liquid 35 s; on-demand boot 1.7x to 4x a cached boot; dedup 77% at 4 KB falling to 59% at 256 KB on 183 images | miss cost stated as "several times longer IO delay" and never measured; no latency numbers anywhere; HDD and 1 GbE |
| DADI (ATC '20) | block-level lazy loading with tree P2P; 10,000 containers on 1,000 hosts in 4 s; trace prefetch removes 95% of the cold gap; reads from a parent's page cache are faster than local disk | no per-read miss latency; not content-addressed |
| Slacker (FAST '16) | only 6.4% of a container image is read at startup; lazy fetch over NFS; run phase 17% slower | no per-block miss cost; centralized |
| VMTorrent (CoNEXT '12), VMThunder (TPDS '14) | demand-priority P2P VM image streaming with recorded profiles | startup seconds only |
| FaaSnap (EuroSys '22), REAP (ASPLOS '21) | lazy page faults from local disk at 13 µs; userfaultfd over 128 µs uncached; working set 9% of footprint | memory, not disk; local |
| SnowFlock (EuroSys '09) | 275 µs per page fetched over gigabit, 82% of it in the network stack | the only in-VM remote per-unit number, and it is from 2009 |
| Dahlin et al. (OSDI '94) | cooperative caching: remote client memory at 1.25 ms against disk at 15 ms; N-chance forwarding | the argument this study repeats at 100 GbE with content-addressed chunks |
| CLB (VEE '17), Satori (ATC '09) | content-keyed sharing of VM disk reads across guests on one host; 95 to 98% of boot reads eliminated | single host; no store |

**Nobody has measured a content-addressed chunk fetched from a peer inside a VM block read path at microsecond scale.**

Every lazy-loading system reports startup seconds, admits a per-read penalty, and hides it with a recorded prefetch profile.

## Transport measurements in prior work

i10 (NSDI '20) and blk-switch (OSDI '21) showed kernel TCP can match RDMA on throughput per core with batching, at a latency cost of 50 to 100 µs at low load.

The SPDK 24.05 reports on ConnectX-5 put kernel nvme-rdma at 12.1 µs and kernel nvme-tcp at 21.4 µs for a 4K read against a null device.

Homa (ATC '21) and eRPC (NSDI '19) put kernel bypass at 2 to 4 µs and attribute the rest of kernel TCP to wakeups and core selection.

No storage paper measured a blocking userspace daemon over kernel TCP as a remote read target; that row is estimated on page 04 and measured here.

## Objections already in print

**Dong et al. (FAST '11)** rejected per-chunk hash placement for backup streams on locality grounds and routed 1 MB super-chunks; page 03 answers with a local cache and page 04 measures the cost.

**Meyer and Bolosky (FAST '11)** already showed deduplication savings grow with the log of the number of machines in one domain, which is the capacity half of hypothesis 2 stated for desktops.

**Jin and Miller (SYSTOR '09)** found fixed blocks match CDC on VM images, which is why part 1 predicts a tie.

**despairlabs (2024)** tells ZFS operators to use clones and block cloning for the copy case and deduplication rarely; the study agrees on one host and disagrees across hosts.

**Every hyperconverged product** (Nutanix, Datrium, SimpliVity, vSAN ESA) mirrors a write over the network before acknowledging it, so a local-only ack is a durability trade, not a free latency win; page 01 makes it a class and page 03 prices both.

## What this study adds

Datrium's patent and Nutanix's design are cited by name, and Fossil and Venti are cited as the origin of the two-tier shape.

The study's contribution is the measurement: what content addressing provides across hosts on commodity hardware under a stock hypervisor, and what the remote cold read costs, per transport.
