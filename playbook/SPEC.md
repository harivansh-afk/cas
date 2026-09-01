# Chunk pointers, not block pointers — research spec v4

NOTE: as of v4.1 the app pages (`src/routes/00–04`) are the authoritative text; this file is design history and no longer tracks wording changes.

Source of truth for the playbook app. Five pages: thesis, model, distribution, measurement, implementation. Typeset verbatim. Labels: hypotheses (H), assumptions (A), rungs (R), stages (S), gates (G).

v4 changes from v3: no reused private code, so the hypervisor is stock and all new code lives in a vhost-user-blk daemon; ZFS reframed as an uncontrolled case study with a new raw-file control rung; the prolly tree replaced by a Merkle-paged map for the dense-key block map, prolly deferred to distribution metadata; write amplification, compactor interference, crash consistency, and the read-heavy aged workload added; the storage-economics objection stated on page 00; register tightened to technical prose.

---

## PAGE 00 — Thesis

Eyebrow: CS 4993 · fall 2026

**Goal of the study.** Measure whether chunk-level content addressing finds enough duplicate data that copy-on-write systems structurally miss to justify its runtime costs. The instrument is a purpose-built two-tier storage backend; the baseline is stock ZFS; the evidence is a redundancy census over real corpora and a four-rung system comparison.

### The structural gap

Two VMs each run `apt upgrade` and download the same packages. Their disks now hold identical bytes. No snapshot, clone, or backing chain can share those bytes, because neither copy descends from the other. Copy-on-write shares data that was copied. It cannot share data that became equal. This study calls the difference cross-lineage redundancy. A content-addressed store captures it because identity is the address; a block-pointer store cannot, regardless of tuning.

The size of that gap on real data is unmeasured. The community's operating rule (despairlabs, 2024) is that dedup pays only when clients cannot or will not issue an explicit copy signal; no measurement of how much sharing the signal misses exists. Granularity studies exist for desktops (Meyer & Bolosky, FAST '11), containers (DupHunter, ATC '20), and models (ZipLLM 2025; Xet production data). None measures the lineage axis. VM fleet data dates to 2009 (Jin & Miller, SYSTOR '09).

### The objection this study must survive

Raw capacity is cheap; NVMe retails on the order of $50–100 per TB. Capturing even half of a small fleet's bytes saves little money at rest. The claim is therefore not "disks get smaller." Captured redundancy is worth measuring because it prices three things at once: capacity (compounding at fleet scale), transfer (sync, migration, and provisioning move unique bytes only), and cache (N guests reading one shared chunk occupy one page-cache entry, not N). The census and the system comparison report all three separately. If all three come back small, the study reports that copy-on-write plus zstd is sufficient, with the numbers to show it.

### Hypotheses

- **H1.** In multi-VM fleets, a substantial fraction of duplicate bytes lies across lineage boundaries. Measured offline on five corpus classes. Falsifiable; a small result reverses the recommendation and still stands as a result.
- **H2.** A two-tier backend, a durable staging log ahead of a content-addressing compactor, captures cross-lineage redundancy with guest-visible write latency comparable to a raw-file backend. The costs relocate to write amplification, compaction bandwidth, and index memory; all three are measured.
- **H3.** Chunk pointers distribute where block pointers do not: a chunk's placement is a function of its name, so the capacity tier spreads across hosts without shared allocation state. Argued from the design; demonstrated on two nodes; not benchmarked further.

### Hardware

The study runs on x86-64 bare metal. This is the architecture of every system in the comparison literature (Meyer & Bolosky, DupHunter, the ZFS deployment base), so results compare directly to prior work.

Primary testbed: two CloudLab c6525-100g nodes (Utah cluster). Per node: one AMD EPYC 7402P, 24 cores at 2.80 GHz, Zen 2; 128 GB ECC DDR4-3200 (8 × 16 GB RDIMM); two 1.6 TB NVMe SSDs, PCIe 4.0; one 25 GbE and one 100 GbE experiment link. One NVMe device holds the system and results; the second is dedicated to the store under test, so guest IO and compaction never share a device with the OS. The 100 GbE pair carries S3. CloudLab allocations are free for sponsored academic research; the sponsor approves the project.

Fallback if CloudLab access is not granted: two OVHcloud Advance bare-metal servers (2026 line): AMD EPYC 4005-series, 16 cores/32 threads, DDR5 ECC, 2 × 960 GB NVMe, 25 Gbps private bandwidth.

No RDMA, no persistent memory, no accelerators on either testbed; the commodity restriction is part of the claim. Every throughput and latency figure in the paper is measured on the testbed. None is quoted from vendors or prior work.

### Assumptions

- **A1.** Workload class: hosts serving multiple guests from local flash, homelab to rack scale. Array economics out of scope.
- **A2.** Experiments run at single-digit TB. Index, amplification, and compaction costs are reported as formulas with measured constants; the 100 TB figures are labeled extrapolations.
- **A3.** Equal BLAKE3 (256-bit) implies equal bytes. A verify-on-dedup arm bounds the risk empirically (Henson, HotOS '03, cited).
- **A4.** The guest contract is virtio-blk with a volatile write cache: an acknowledged FLUSH is durable, nothing else is.
- **A5.** One image, one writer. Shared-disk clustering out of scope.
- **A6.** Dedup side channels and convergent-encryption probing are documented and excluded; the store is trusted infrastructure here.
- **A7.** Compression is zstd, measured in both orders relative to dedup. All-zero and unallocated ranges are excluded from every ratio and reported separately.
- **A8.** Corpora represent their declared classes only. Build scripts are published; results are per class; no universal ratio is claimed.

---

## PAGE 01 — The model

**The system is an LSM tree whose compaction step is content addressing.** Writes land in a tier that ignores content. A background pass moves settled data into a tier organized by nothing else.

### Datapath

The guest sees a virtio-blk device. The hypervisor is stock QEMU; it connects the device to an external process over the vhost-user-blk protocol. All new code lives in that process, the daemon. Guest memory is shared with the daemon, so requests are read in place. The daemon issues storage IO through io_uring. No hypervisor is forked or patched (provenance on page 04).

### Write tier

Guest writes append at block granularity to a staging log on NVMe. The hot path performs no hashing and no chunking; large writes proceed at sequential-append speed. FLUSH is fdatasync of the staging log, then the acknowledgment. Durability belongs to the staging log alone: the page cache may serve reads, but it is never the durability mechanism, and host RAM is never the write buffer. The log is disk-backed, so the buffer is durable and has a defined size.

### Compactor

A background pass reads settled extents from staging, cuts them with content-defined chunking (FastCDC), hashes each chunk with BLAKE3, writes unique chunks to the capacity tier, and updates the map. Extents overwritten in staging are never compacted; superseded chunks become garbage. CDC runs only here: on the hot path, a single offset-aligned write can move the content-defined boundaries of its neighborhood, so inline CDC on a block device is incoherent rather than merely slow.

The design buys its write path with two known costs. First, write amplification: every surviving byte is written at least twice (staging, then chunk store) plus map-journal traffic; the measured WA factor is a headline number, not a footnote. Second, interference: compaction reads staging and writes the store on the same device the guest is using; the S2 benchmarks measure guest p99 with the compactor active and idle, and the delta is reported.

### Capacity tier

Three structures. The chunk store is an append-only log of records (length, hash, flags, bytes) and is authoritative. The index maps hash to location, resides in RAM, is rebuilt by scanning the store, and is never authoritative; its bytes-per-TB constant feeds the A2 extrapolation. The map, one per image, is an ordered structure from disk offset to chunk hash, journaled, with copy-on-write snapshots.

**Map arms.** The controlled experiment inside the daemon is the map structure. R2 uses a conventional offset tree, the same shape as a block-pointer tree, pointing at chunks. R3 uses a Merkle-paged map: the flat offset array is divided into fixed pages, each page hashed, with a hash tree over the pages. Because block-map keys are dense integers, this structure delivers the two properties history-independent metadata is for, diffs proportional to the changed pages and whole-image verification by root hash, without a prolly tree's machinery. A prolly tree generalizes the same properties to sparse variable keys; that case arises in distribution metadata and is deferred to phase 2 (page 02).

### Read path

Reads check staging, then the map, then the store. Fresh data is served from the raw log without indirection. Settled data pays the map walk, the index lookup, and, as the store fragments, seek amplification. This is the workload the design is worst at: a read-heavy process over settled data pays the indirection on every access. S2 includes that workload deliberately, young and aged.

### Crash consistency

Two logs exist, staging and the map journal, and they must agree after a crash. Ordering rule: staging is senior. Compaction is idempotent (re-chunking the same extents yields the same hashes), and every compaction batch carries an epoch number recorded in both logs. On recovery: replay the staging log; discard map-journal records from any epoch whose staging extents were not yet marked compacted; re-run compaction from the oldest incomplete epoch. `kill -9` at any point followed by this replay must pass `fio --verify`; that is gate G4, not an aspiration.

### Chunking debt

Staging is finite. If sustained ingest exceeds compaction bandwidth, staged bytes accumulate until back-pressure throttles the guest. The sustainable ingest ceiling and the point where back-pressure engages are measured in S2. On this testbed the compactor is expected to be IO-bound rather than hash-bound; the measurement confirms or refutes that.

### Garbage

A chunk is live if staging or any map references it. Collection is mark-and-sweep: scan the maps, build a live set, punch holes (`FALLOC_FL_PUNCH_HOLE`) over dead records. No reference counts; refcount maintenance is the classic dedup write tax, and the maps are small enough to scan. No reclamation inside an open snapshot epoch.

### Host filesystem

The daemon's files (staging log, chunk store, maps, journals) reside on XFS on the dedicated NVMe device, opened with O_DIRECT in the media-honest arms. XFS is required, not preferred: garbage collection is `FALLOC_FL_PUNCH_HOLE`, so the backing filesystem must support hole punching with extent-based allocation, and the store needs working O_DIRECT and io_uring semantics. A raw partition would remove filesystem interference but removes hole punching with it. ZFS never sits under the daemon; stacking two copy-on-write systems would confound every measurement.

### The rungs

Same stock QEMU configuration for all four; only the storage behind the device varies. R0, R2, and R3 share one XFS filesystem on the dedicated NVMe.

- **R0 — raw file on XFS.** The control. No dedup, no ZFS, no daemon features beyond passthrough.
- **R1 — raw file on a ZFS zvol, stock OpenZFS, its own pool on the same NVMe device.** Configuration: `checksum=blake3, dedup=on`; `volblocksize` matched to the daemon's chunk-size arm, because zvol dedup granularity is the volblocksize; compression off outside the labeled compression arm; DDT memory read from `zpool status -D` and reported in the same index-cost column as the daemon's. The incumbent block-pointer design with content-hash identity. R1 is a case study, not a controlled comparison: it differs from the daemon in kernel boundary, caching, and allocation, and the paper attributes cross-rung deltas accordingly. Patching ZFS is out of scope; a fork would consume the schedule and demonstrate nothing stock ZFS does not.
- **R2 — the daemon, offset-tree map.** Chunk-level content addressing, conventional metadata.
- **R3 — the daemon, Merkle-paged map.** Chunk-level content addressing, history-independent metadata.

R0 versus R2 prices content addressing. R2 versus R3 prices the metadata structure. R1 anchors both against the deployed state of the art. Controlled claims are made only within the daemon rungs.

### Figure 1 — the two-tier datapath

Guest and QEMU on the left, connected over vhost-user to the daemon. Staging log center with the durability boundary marked (FLUSH → fdatasync → ack). Compactor beneath (FastCDC · BLAKE3), fed by settled extents, emitting unique chunks. Capacity tier right: map, index, chunk store; cold-read fallthrough drawn staging-first; sweep drawn dashed.

### Figure 2 — block pointers versus chunk pointers

Side by side. Left: a block-pointer tree (fixed records, offsets as identity, a refcounted dedup table bolted on, sharing only along clone lineage). Right: the chunk-pointer map (variable chunks, hashes as identity, sharing wherever content coincides, no reference counts). One caption line: the left structure shares what was copied; the right shares what is equal.

---

## PAGE 02 — Distribution

**Local write, global dedup.** The write path never crosses the network: staging is a local log on the host running the guest, so ingest latency is a local NVMe property at any cluster size. Content addressing becomes global at compaction, which is already asynchronous.

### What the name buys

Chunks are immutable and named by content, so placement is a function of the name: rendezvous or CRUSH-style hashing from chunk hash to k owner nodes. No allocation tables, no rebalancing metadata, no coordinator on the data path. The index partitions by the same function, so the shard owning a chunk owns its index entry; routing and lookup are one computation. Any node may cache any chunk, and caches converge cluster-wide because names are global. Scrub is re-hash; a corrupt replica is detected by name and repaired from any peer.

Compaction ships a chunk only if the owning shard lacks it. Cluster ingest traffic is therefore proportional to unique bytes, not written bytes.

### What stays hard

- Maps are mutable and follow their writer (A5): the map lives with the guest's host and moves when the guest does. Data placement is content-shaped; map placement remains lineage-shaped.
- Global liveness requires roots from every map owner. Epoch-based collection, roots gathered per epoch, no reclamation inside an open epoch. Designed here, validated only at two nodes.
- A cold read whose chunk lives remotely pays a network round trip inside guest latency. Staging absorbs writes and recent reads; caching absorbs part of the remainder; the residue is the true cost of disaggregation and is the designated follow-on study.
- Partitioning places the index; it does not shrink it. Cluster-scale honesty depends on the per-TB constants measured in S2.

### Transport

The transport is a phase-2 decision and the follow-on study's subject. Candidates, with the tradeoff each represents: kernel TCP with per-core connections and batched submissions, the i10 design (NSDI '20), which reached RDMA-class CPU efficiency without kernel bypass; nvme-tcp, its standardized kernel descendant, which presents remote chunks as block namespaces; QUIC with one stream per in-flight chunk, which removes head-of-line blocking across concurrent fetches at a userspace per-byte CPU cost. Choosing among them requires exactly the per-stage measurement methodology this study builds, applied at the fabric, which is why the transport question is deferred rather than guessed.

### Figure 3 — the wire

Two hosts. Host A: guest, staging log (marked local, never networked), compactor. Compactor output fans to shard owners by hash prefix, host A and host B each owning a range. Only unique chunks cross the wire, labeled as such. The transport segment drawn as a labeled slot with the three candidates listed beside it, marked "phase 2: measured, not guessed." Host B mirrors the structure to show symmetry; no master.

### Scope

This page argues H3 and demonstrates it at two nodes: placement lands chunks by hash; a fleet sync transfers bytes proportional to unique bytes (gate G3). Everything further is phase 2.

---

## PAGE 03 — Measurement

Three stages. Each gates the next and ends with a standalone result.

### S1 — Redundancy census (H1, weeks 1–4)

Offline analysis of images at rest. No VMM, no daemon, no root. First numbers in two weeks.

Corpora (A8 scripts for each): cloned fleet (golden image, N clones, scripted drift; lineage's best case). Convergent installs (N independent installs updated to the same package set; lineage's structural blind spot). Container layer stacks (cross-checks DupHunter at chunk granularity). Model family on the testbed (base, fine-tunes, quantizations; where whole-file dedup collapses). Nix store generations (successive closures of one flake; no published study).

Method. Chunk every image at whole-file, CDC, and fixed granularities. Compute duplicate bytes. Against each corpus's declared ancestry, split them: lineage-capturable, defined as identical and in-place relative to an ancestor (the ceiling for any COW system), plus a simulated COW at realistic record sizes and a declared snapshot cadence, since sibling sharing depends on when snapshots were taken; the remainder is cross-lineage, reachable only by content. Compression in both orders per A7; zeros excluded per A7.

The census settles standing claims as a side effect: whether compression captures most of dedup's win; whether fixed blocks still approximate CDC on VM images (the 2009 result, retested); whether whole-file dedup collapses on model corpora; and what fraction of observed sharing an explicit copy signal could ever have declared.

### S2 — System comparison (H2, weeks 5–12)

The four rungs on identical workloads, guest-visible metrics as the common denominator.

Workloads: fio (4K random write/read, 128K sequential, QD 1/8/32); kernel untar and build in the guest; N-clone boot storm; a read-heavy pass over settled data (the design's worst case, run young and aged); replay of the S1 fleet corpora.

Measured per rung: guest p50/p99 write and read latency, compactor active and idle; write amplification (device bytes written per guest byte, from NVMe counters); storage consumed after ingest and after compaction settles; sustainable ingest ceiling and the back-pressure point; compaction bandwidth; index bytes per stored TB; recovery, `kill -9` then replay then `fio --verify`.

Instrumentation: per-request stage timestamps inside the daemon, drained to ndjson, cross-checked once against bpftrace with the delta reported. ZFS is observed at the guest boundary plus `zpool` statistics; its internal stages are not comparable to the daemon's and the paper does not equate them. Controls: pinned vCPUs, performance governor, discarded warm-up, at least five repetitions, variance printed beside every number.

### S3 — Distribution demonstration (H3, stretch)

Two nodes on the existing link: placement by hash lands chunks on the correct owner; a fleet sync transfers bytes within G3's bound of unique bytes. Nothing further.

### Gates

- **G1.** The census decomposition is exhaustive and disjoint; categories sum to 100% of non-zero bytes per corpus.
- **G2.** The comparison table is complete: four rungs, identical workloads, latency, amplification, storage, and index columns, no empty cells.
- **G3.** Two-node sync bytes within 10% of unique bytes.
- **G4.** Recovery passes `fio --verify` after `kill -9` at arbitrary points, all rungs that involve the daemon.
- **G5.** One command reruns every experiment on a second machine.

### Schedule

| Weeks | Stage | Result |
|---|---|---|
| 1–2 | S1 | census pipeline; cloned-fleet and convergent-install splits |
| 3–4 | S1 | remaining corpora; H1 verdict |
| 5–6 | S2 | daemon skeleton over vhost-user; staging tier; R0 baseline runs |
| 7–9 | S2 | compactor, store, offset map; R2 runs; R1 (ZFS) configured and run |
| 10–11 | S2 | aged and read-heavy runs; debt ceiling; WA and interference |
| 12 | S2 | Merkle-paged map; R3 runs; comparison table complete |
| 13–14 | — | report; reproducibility pack |
| stretch | S3 | two-node placement and sync |

### Logistics and risks

CS 4993, 1 credit for registration. Planned effort is roughly 8 hours weekly; the credit understates the work and this document does not. Expectations in writing before Sep 9; thirty minutes of sponsor time biweekly.

Risks. Corpus bias is the principal threat to H1; A8 is the mitigation, and the corpus scripts are published so the classes themselves can be criticized. Daemon overrun is the principal threat to H2; the cut order is fixed in advance: R3 first, then the aging protocol, never the R0/R2 comparison or the census. The lineage-vs-content novelty claim was checked against the open web (2026-09-01) but not against OpenZFS development talks and mailing lists; those are swept before related work is final.

---

## PAGE 04 — Implementation

### What is stock and what is new

Every line the study's claims depend on is either a stock upstream release or new code in one repository. The hypervisor is never forked: QEMU speaks vhost-user-blk to an external process, so all new code lives in that process, the daemon.

| Component | Source | License |
|---|---|---|
| Hypervisor | stock QEMU (vhost-user-blk front end), unmodified | GPL-2.0, unmodified use |
| vhost-user protocol handling | rust-vmm `vhost-user-backend`, `vm-memory`, `virtio-queue` crates | Apache-2.0 |
| BLAKE3 | official `blake3` crate | Apache-2.0/CC0 |
| CDC | `fastcdc` crate, or reimplemented from the FastCDC paper if the crate falls short | MIT |
| ZFS rung | stock OpenZFS ≥ 2.2 | CDDL, unmodified use |
| Staging log, compactor, chunk store, index, maps, GC | written for this study | ours |
| Census pipeline, harness, analysis | written for this study | ours |

This split is what makes the measurements defensible. Because the hypervisor is unmodified, no result can be an artifact of a patched QEMU, and the R0 control runs the identical binary. It also bounds the build: the protocol plumbing comes from maintained crates, so the engineering budget is spent entirely on the five components the paper is about.

### Repository

```
chunkd/
  crates/
    daemon/        # vhost-user-blk backend: request loop, staging, FLUSH
    staging/       # append-only staging log, replay
    compact/       # FastCDC + BLAKE3 pass, epochs, back-pressure
    store/         # chunk log, index, hole-punch GC
    map/           # offset-tree and Merkle-paged map arms, journal, COW snapshots
    trace/         # per-request stage timestamps -> ndjson
  census/          # S1: corpus build scripts, chunkers, decomposition, folk-claim checks
  harness/         # rung configs (R0-R3), fio jobs, workloads, runner, aging protocol
  analyze/         # tables and figures from ndjson; uv-run python
  results/         # tagged ndjson and figures per run
  docs/            # this spec, methodology notes
```

### Build order

The census (`census/`) is standalone and starts on day one. The daemon starts as passthrough (staging tier only, R0-equivalent behavior) to validate the vhost-user path against stock QEMU before any content addressing exists. The compactor and store land next (R2), then the ZFS rung configuration, then the second map arm (R3). Each rung is benchmarkable the week it lands; no step depends on a later one.
