# casblk — research spec

Source of truth for the playbook artifact. Style: ASD-STE100 principles. Active voice. Present tense. One instruction per sentence. One term per concept: chunk, record, chunk log, index, block map, backend, guest, request, stage, arm.

Requirement IDs are stable. Do not renumber.

---

## PAGE 00 — Overview

Eyebrow: CS 4993 · measurement study · fall 2026

**casblk measures the latency cost of content-addressed storage on the guest disk path.** The system is the instrument. The paper is the product.

### Claims

The paper makes three claims. Every design decision serves one of them.

- **C1 — Taxonomy.** A per-stage latency breakdown of a virtio-blk request over a content-addressed backend. The taxonomy attributes ≥ 90% of the p99 gap between the raw-file backend and the CAS backend.
- **C2 — Tradeoff curve.** Dedup ratio against tail latency, across chunk sizes and hash functions.
- **C3 — Async-hash result.** The latency recovered when the hash moves off the critical path, and the size of the integrity window this opens.

### Functional bar

The instrument is trustworthy when four checks pass. Nothing more is required.

- **F1.** The backend boots a stock Linux guest.
- **F2.** The backend implements the five virtio-blk request types.
- **F3.** `fio --verify` passes on every arm.
- **F4.** The store recovers after `kill -9`: rescan the log, rebuild the index, pass verify.

### Out of scope

GC during benchmarks. Live migration. Multi-host operation. Security hardening. Snapshot trees (one COW copy only). qcow2 inside the VMM.

### Novelty (checked 2026-08-31)

Dedup latency studies measure the backend on bare metal (iDedup FAST'12, Dmdedup OLS'14, VDO TOS'24). Virtio studies measure the transport over plain backends (Spool ATC'20, LightIOV). No published work measures the intersection per stage. Closest: CLB (VEE'17) uses content addressing as an optimization; it does not measure its cost.

### Context

CS 4993, 1 credit, ~3 h/week, 14 weeks. Sponsor: Cai (latency framing) or Cheng (dedup framing). Expectations in writing before Sep 9.

---

## PAGE 01 — CAS store

The store has three components. The chunk log holds the data. The index locates the data. The block map gives each disk image its view of the data.

### Chunk log

- **CAS-1.** The chunk log is an append-only binary file. It is the only authoritative structure in the store.
- **CAS-2.** Each record has a fixed header: magic, length, hash, flags. The chunk bytes follow the header.
- **CAS-3.** The log is also the write-ahead log. A record is durable after `fdatasync`.
- **CAS-4.** *(stretch)* Reclamation punches holes (`FALLOC_FL_PUNCH_HOLE`) over dead records. The log is never compacted. The filesystem reclaims the space.

### Index

- **CAS-5.** The index maps hash → log offset. It lives in RAM.
- **CAS-6.** The index is a cache of the log. A rescan of the log rebuilds it. The index is never authoritative.
- **CAS-7.** A periodic index snapshot makes restart fast. A stale snapshot is safe: replay the log tail.
- **CAS-8.** A flash-resident index is an experiment arm, not the default.

### Block map

- **CAS-9.** Each disk image has one block map: a flat mmap'd array with one entry per chunk-sized extent. Each entry is a 32-byte hash.
- **CAS-10.** Map updates append to a map journal. A checkpoint writes the full array and truncates the journal.
- **CAS-11.** A snapshot is a COW copy of the map.
- **CAS-12.** The zero chunk has a well-known hash and no storage. WRITE_ZEROES sets map entries to the zero chunk. DISCARD sets map entries to unmapped.

### Integrity

- **CAS-13.** Invariant: for every map entry, `BLAKE3(chunk bytes) == entry hash`.
- **CAS-14.** Debug builds verify the hash on every read. Release builds verify during scrub.
- **CAS-15.** Dedup trusts hash equality (BLAKE3, 256 bit). A verify-on-dedup byte compare is an experiment arm. Cite Henson, HotOS '03.

### Dedup and liveness

- **CAS-16.** The chunk log and the index are global. Block maps are per-image. Two images that write the same bytes share one record. Cross-VM dedup needs no extra mechanism.
- **CAS-17.** A chunk is live when at least one block map references it. The block maps are the only liveness roots.
- **CAS-18.** *(stretch)* GC is mark-and-sweep: scan the maps, build a live bitmap, punch the dead records. No refcounts.
- **CAS-19.** No reclamation occurs inside an open snapshot epoch.

### Parameters

Chunk size ∈ {4K, 16K, 64K}. A guest write smaller than one chunk causes a read-modify-write. The study measures this cost; it does not hide it. Hash ∈ {BLAKE3, SHA-256}.

---

## PAGE 02 — VMM integration

- **VMM-1.** The backend plugs into the existing rust-vmm VMM behind one backend trait. Two implementations ship: raw-file and cas.
- **VMM-2.** The backend implements READ, WRITE, FLUSH, DISCARD, WRITE_ZEROES. GET_ID is trivial.
- **VMM-3.** Descriptor chains point into guest RAM. The VMM maps guest memory. The backend reads and writes guest buffers directly. No copy at the boundary.
- **VMM-4.** The device reports a volatile write cache. Data is durable only after an acked FLUSH. These are qemu `cache=writeback` semantics. The guest kernel already understands them.
- **VMM-5.** The backend drains a batch of requests per ring notification. In-flight requests run up to the queue depth.
- **VMM-6.** File IO goes through io_uring.
- **VMM-7.** All comparison arms run inside this VMM: raw-file vs cas, plus raw-file on a VDO device as the kernel-inline-dedup arm. qemu/qcow2 numbers are context only; a cross-VMM comparison mixes VMM variables into the measurement.

---

## PAGE 03 — Hot paths

The stages are the measurement units. The write path defines timestamps T0–T7. Every latency claim in the paper decomposes into these stages.

### Stage timestamps

| Stamp | Event |
|---|---|
| T0 | request popped from the ring |
| T1 | descriptor chain parsed, extents split |
| T2 | chunk hash complete |
| T3 | index lookup complete (hit or miss) |
| T4 | log append submitted (io_uring) |
| T5 | log write complete |
| T6 | block map + journal updated |
| T7 | ack pushed to the ring |

Read path analog: T2r index lookup, T3r log read submitted, T4r read complete, T5r verify (debug), T6r ack.

### Write path

1. Pop the request. Parse the descriptor chain. *(T0→T1)*
2. Split the write into chunk-aligned extents.
3. For a full-chunk extent: hash the bytes. *(T2)*
4. Look up the hash in the index. *(T3)*
5. On hit: write the hash into the block map. Append one map-journal record. Skip to step 8.
6. On miss: append the record to the staging buffer. Submit the log write. *(T4, T5)* Insert the index entry.
7. For a partial-chunk extent: read the old chunk, patch the bytes, go to step 3. This is the RMW path.
8. Update the map. *(T6)* Ack the request. *(T7)* Data may stay volatile until FLUSH.

### Read path

1. Look up the map entry.
2. Zero chunk: return zeros. Unmapped: return zeros.
3. Otherwise: index → log offset → read the chunk.
4. Debug build: verify the hash.
5. Ack.

### FLUSH

1. `fdatasync` the chunk log.
2. `fdatasync` the map journal.
3. Ack. An acked FLUSH means durable. Nothing else does.

### DISCARD

Set the map entries to unmapped. Append journal records. Ack. The sweep reclaims the space later (CAS-18).

### Async-hash arm (C3)

1. Append the bytes to the log. Ack after the append. The log is the WAL, so no data is lost.
2. A worker hashes the bytes off the critical path.
3. The worker inserts the index entry and dedups retroactively. A late duplicate rewrites the map entry; the sweep reclaims the orphan record.
4. The integrity window is the ack-to-hash interval. The harness measures it. This is the inline vs post-process dedup distinction from the literature, measured per stage.

### Diagram

One SVG. Swimlanes left to right: Guest kernel → virtio ring → backend dispatch → CAS engine (hash, index, staging) → storage (chunk log, map journal). Mark T0–T7 at the lane boundaries. Show the hit/miss branch at the index. Show FLUSH as a dashed path. Monochrome on black with one accent. No decoration; every element is a real component or a real transition.

---

## PAGE 04 — Measurement system

- **MEAS-1.** The backend records T0–T7 per request into a lock-free ring buffer. A drain thread writes ndjson. Cost per stage: one `clock_gettime` / `rdtsc`.
- **MEAS-2.** A one-time cross-check compares internal timestamps against bpftrace probes. The report states the delta.
- **MEAS-3.** Arms: backend {raw, cas, raw+VDO} × chunk {4K, 16K, 64K} × hash {BLAKE3, SHA-256} × hash mode {inline, async} × index {RAM, flash}. Run a chosen subset per claim, not the full cross product.
- **MEAS-4.** Benchmarks, three, and they double as the functional proof (F1–F3):
  1. **fio micro:** 4K randwrite, 4K randread, 128K seq write; QD ∈ {1, 8, 32}.
  2. **Macro:** Linux kernel untar + defconfig build inside the guest; guest boot time.
  3. **Dedup corpus:** ≥ 3 distro images plus 2 drifted clones written into one store; cross-image dedup ratio.
- **MEAS-5.** Metrics: per-stage p50/p99/p999, IOPS, dedup ratio, index bytes per stored TB, write amplification.
- **MEAS-6.** Controls: pinned vCPUs, performance governor, discarded warm-up run, ≥ 5 repetitions, variance reported next to every number.
- **MEAS-7.** Validation gates, each mapped to a claim:
  - **V1 (C1).** The stage sums account for ≥ 90% of the measured p99 gap, raw vs cas.
  - **V2 (C2).** The curve holds ≥ 3 chunk sizes × 2 hashes.
  - **V3 (C3).** The async arm's p99 distance from raw is reported with the measured integrity window. Characterized, not promised.
  - **V4 (F3).** `fio --verify` is clean on every arm.
  - **V5.** One command reruns the full harness on a second machine.

---

## PAGE 05 — Repo and plan

### Tree

```
casblk/
  crates/
    chunkstore/    # log, index, BLAKE3, hole-punch reclaim; no virtio deps
    blockmap/      # flat map + journal, COW snapshot
    backend/       # backend trait: raw-file impl, cas impl
    trace/         # T0–T7 ring buffer → ndjson
  vmm/             # thin glue into the existing rust-vmm VMM
  harness/
    workloads/     # fio jobfiles, kernel-build script, boot-time
    corpus/        # distro image fetch scripts
    runner/        # one command per arm; tagged ndjson out
    analyze/       # uv-run python: taxonomy plots, tradeoff curve
  results/         # committed ndjson + figures per tagged run
  docs/            # design.md, methodology.md
```

### Milestones

| Weeks | Work | Gate |
|---|---|---|
| 1–2 | Harness + raw-file baseline. Preliminary numbers for the pitch. | F1, first p99s |
| 3–5 | CAS backend wired and instrumented. First taxonomy. | F2, F3, C1 draft |
| 6–9 | Sweep chunk, hash, index arms. Build the curve. | V1, V2 |
| 10–12 | Async-hash experiment. | V3 |
| 13–14 | Report + reproducibility pack. | V4, V5 |

### Stretch goals

Discard-driven mark-and-sweep GC with hole punch (CAS-18). Prolly-tree block map for delta-proportional image sync. Verify-on-dedup arm (CAS-15).

### Risks

Integration overrun in weeks 3–5 is the main risk. Fallback: a fixed-4K-only CAS backend keeps C1 and C3 and drops part of C2's curve. Agree to this fallback with the sponsor in writing.

### Distribution note

The data plane distributes later with a CRUSH-style placement function over the hash; chunks are immutable and location-independent. The mutable pieces (maps, liveness) and remote-read p99 are the follow-on project, not this one.
