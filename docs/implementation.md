# Implementation plan

Status: staging reference and Nix test environment, 2026-09-05. Research baseline: specification v10 at
`74c8976`. This document tracks implementation; it does not revise the paper's
hypotheses or mark its gates as passed.

The implementation will test whether content-addressed chunks let VM hosts
share capacity, transfer manifests, and serve repeated reads from peer memory.
The study remains bounded to two Linux hosts with static membership.

Terms follow [the specification](spec.md): E is the contiguous confirmed append
prefix, D the durable manifest/store prefix, and O the prefix durable at every
owner. `O <= D <= E`. R0 is QEMU's raw-file-on-XFS control; R3 is the backend.

## Starting point and structure

The repository initially contained the specification, source reviews, and the
paper website. The implementation uses Rust for the storage path and Python
through `uv` for experiment orchestration and analysis. There is no Python
process on the guest IO path.

Keep the workspace small. `cas-core` owns storage semantics and their tests.
`cas-cli` provides executable checks and will acquire census and administrative
commands. Add `cas-daemon` when the vhost-user adapter starts; isolate Linux
request handling, queue ownership, and io_uring there. Add core modules for
chunking, manifests, compaction, placement, and protocol as each milestone needs
them. The shared chunker must be used by both census and compactor.

`experiments/` holds environment capture, guest definitions, workload generation,
run orchestration, and analysis. `results/` holds ignored local artifacts. Keep
the existing `playbook/` as the publication layer; it consumes established
results rather than acting as the experiment runner.

## Dependencies

Direct dependencies installed in this iteration are resolved in `Cargo.lock`.
The future dependency versions below are recommendations checked against
primary sources on 2026-09-05; they have not been compiled together here.

| Package | Purpose | When |
|---|---|---|
| `libc` 0.2 | Linux direct-IO open flags | Installed |
| [`crc32fast` 1.5](https://docs.rs/crc32fast/latest/crc32fast/) | IEEE CRC32 integrity checks for staging records; separate from content IDs | Installed |
| `thiserror` 2 | Typed storage errors | Installed |
| [`clap` 4.6](https://docs.rs/clap/latest/clap/) | Command-line arguments | Installed |
| [`serde`](https://serde.rs/) 1, [`serde_json`](https://docs.rs/serde_json/latest/serde_json/) 1 | Versioned JSON artifacts and later configurations | Installed |
| [`tempfile` 3](https://docs.rs/tempfile/latest/tempfile/) | Temporary filesystem fixtures | Test dependency |
| [`blake3` 1.8](https://docs.rs/blake3/latest/blake3/) | Chunk content IDs and verification of fetched chunks | Census and compactor |
| [`fastcdc` 5, `v2020`](https://docs.rs/fastcdc/latest/fastcdc/v2020/index.html) | CDC arm, through one shared 4 KiB snapping adapter | Census and compactor |
| [`vhost-user-backend` 0.23 / `vhost` 0.17](https://github.com/rust-vmm/vhost/blob/main/vhost-user-backend/Cargo.toml) | QEMU protocol and queue lifecycle | Next iteration; recovery gap below |
| [`vm-memory` 0.18, `virtio-queue` 0.18, `virtio-bindings` 0.2.7, `vmm-sys-util` 0.15](https://github.com/rust-vmm/vhost/blob/main/Cargo.toml) | Guest memory, descriptors, feature definitions, eventfds | Pin as a compatible rust-vmm family |
| [`io-uring` 0.7](https://docs.rs/io-uring/latest/io_uring/) | Asynchronous file IO, fdatasync completions, and TCP | Daemon |

The daemon will use the low-level io_uring crate. Its fsync opcode exposes the
data-sync flag. No general async runtime is needed for the initial event loop.
[io_uring Fsync API](https://docs.rs/io-uring/latest/io_uring/opcode/struct.Fsync.html)

System dependencies for R0 and the daemon are stock QEMU/KVM, `qemu-img`, fio,
XFS tools, a C linker, and Linux inspection tools. R1 adds OpenZFS 2.3 with fast
dedup and the matching kernel module. The RDMA probes add `nvme-cli`, `rdma-core`,
perftest, and kernel nvmet support when the reserved hardware is available.
R2 dm-vdo and the ibverbs daemon arm follow the specification's optional scope.
Versions and host settings are recorded per run rather than inferred from an
installation command.

Start the experiment scripts with Python's standard library. Add NumPy and
Matplotlib through a locked `uv` project when the analysis and publication
figures exist. No database server or service is required for result storage.

## Iteration 1: staging reference

Implemented:

- Contiguous confirmation tracking with exhaustive completion orders for four
  appends and a max-based negative control; monotonic E/D/O validation.
- A Linux staging log with 4 KiB-aligned O_DIRECT buffers, an exclusive advisory
  file lock, ordered writes, zero ranges, and explicit FLUSH fences.
- FLUSH acknowledgment after `fdatasync`; recovery through the last valid fence;
  last-write-wins reads; CRC validation; refusal to overwrite an existing log.
- Empty discard handling that allocates no sequence number. Large zero ranges
  occupy one record and remove only mapped data from the reference index.
- A JSON-emitting `casctl staging-check`, environment inventory, and Rust checks
  configured for the existing GitHub mirror.

The staging path uses synchronous positional IO. A mutable log handle serializes
appends and FLUSH. The independent `DurablePrefix` tracker is tested for future
out-of-order completions; it is not yet wired into an asynchronous device.
The log uses D = O = 0 because there is no store or compactor.

The provisional on-disk format is little-endian:

| Region | Encoding |
|---|---|
| File header | 4 KiB; `CASLOG01`, image byte length, record size, CRC32 |
| Record metadata | 4 KiB; `CASREC01`, kind, sequence, byte offset, byte length, CRC32 over metadata and payload |
| Record payload | 4 KiB; one guest block for WRITE, zeros for ZERO and FENCE |

Header fields occupy offsets 0, 8, and 16. Record fields occupy offsets 0, 8,
16, 24, and 32. The checksum occupies bytes 4092–4095; unused bytes are zero.
Kinds are WRITE = 1, ZERO = 2, FENCE = 3. WRITE and ZERO advance the sequence;
FENCE records the preceding sequence. The file header does not consume a
sequence. All records occupy fixed 8 KiB slots after the file header.

Fixed slots make fence discovery independent of guest-controlled payload bytes.
The reference writes 8 KiB per 4 KiB data block plus 8 KiB per nonempty FLUSH
batch. This is encoded size, not a measurement of device write amplification.
Record packing and batched IO must be evaluated before G1; this layout is not a
frozen performance design. Read/write requests are capped at 1 MiB; a zero range
can span the image without allocating a payload for each block.

Recovery first finds the last complete checksum-valid fence, then validates and
replays every record through it. It rejects a corrupted committed prefix before
changing the file. It truncates the uncommitted suffix and syncs the resulting
file. Reopen is therefore a recovery operation, not a read-only inspection.
Complete unacknowledged batches may survive, as permitted by a volatile write
cache. There is no promise of multi-block write atomicity.

A valid fence establishes the replay bound under the crash/torn-write model.
CRC32 is not authentication. Arbitrary media corruption that destroys the final
fence cannot always be distinguished from a torn unacknowledged fence; this
format does not provide an independent durable copy of the acknowledged E.
Power-cut testing, sync-error injection, and the final recovery format remain
part of the durability work.

Each new file's header is synced and its parent directory is fsynced before
creation returns. Direct IO alone does not establish durability, and fsync on
the file does not establish directory-entry persistence.
[Linux open(2)](https://man7.org/linux/man-pages/man2/open.2.html),
[Linux fsync(2)](https://man7.org/linux/man-pages/man2/fsync.2.html)

The checks cover close/reopen, a killed writer process, truncated and zero-padded
tails, committed-payload corruption, invalid ranges, single-writer exclusion,
forged fence bytes inside guest payloads, and sequence-prefix ordering. They ran
on Spark's aarch64 Linux/ext4 development filesystem. They do not establish G1,
the QEMU multiqueue condition, guest `fio --verify`, or power-loss recovery.

The [Nix test environment](testbed.md) now provides a pinned toolchain, a raw-disk
QEMU/KVM guest with fio verification, and a bare-metal host template. The raw
guest uses io_uring through stock QEMU; our staging library remains synchronous.

Not implemented: existing-image import, a compacted base, timed background
fdatasync, bounded staging/governor, the daemon's io_uring and QEMU adapters,
chunk store, census, remote protocol, snapshots, or migration. No paper
measurement is reported.

## Immediate next iteration: R0 and the QEMU adapter

Use the same guest definition for R0 and R3. Before a performance run, capture
the dedicated drive's read and fdatasync times, CPU pinning, cache settings, and
the exact software versions. The environment inventory here is only the first
part of that run record.

The first device implementation accepts reads, writes, FLUSH, DISCARD, and
WRITE_ZEROES through split virtqueues. Add queue-wide completion accounting,
aligned bounce buffers with a counter, and io_uring submission/completion. Keep
the compactor off the device's FLUSH lock when it is introduced.

Two source findings affect this work:

1. The current rust-vmm framework returns unsupported from both inflight-FD
   handlers and explicitly says backends must not negotiate that feature.
   QEMU has inflight tracking and reconnect machinery. Decide between a narrow
   framework extension and a lower-level handler using `vhost`; record and test
   the choice before claiming restart transparency.
   [rust-vmm handler](https://github.com/rust-vmm/vhost/blob/main/vhost-user-backend/src/handler.rs),
   [QEMU block frontend](https://github.com/qemu/qemu/blob/master/hw/block/vhost-user-blk.c)

2. A reported `blk_size` of 4096 does not change virtio sector units: requests
   still use 512-byte sectors. Linux uses `blk_size` as its queue's logical
   block size. Validate that behavior in the guest, reject nonaligned requests
   explicitly, and check buffer alignment separately. The Cloud Hypervisor
   reference advertises 512 and needs adaptation.
   [Virtio 1.2 block device](https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html#x1-2390002),
   [Linux virtio block driver](https://github.com/torvalds/linux/blob/master/drivers/block/virtio_blk.c),
   [Cloud Hypervisor reference](https://github.com/cloud-hypervisor/cloud-hypervisor/blob/main/vhost_user_block/src/lib.rs)

Inflight replay must preserve the order of staging writes across descriptor
resubmission. Inject crashes around descriptor consumption, append completion,
fdatasync, status publication, and used-ring publication. Reconnecting the
socket alone does not establish request recovery.
[QEMU inflight protocol](https://www.qemu.org/docs/master/interop/vhost-user.html#inflight-i-o-tracking)

## Remaining implementation sequence

| Milestone | Deliverable and acceptance condition | Paper gate |
|---|---|---|
| R0 and passthrough | Same guest workloads on raw XFS and the QEMU daemon; passthrough p99 within 10% of R0 | G1, pending |
| Local storage correctness | Store/index, journaled manifest, D/O advancement, settled compaction, finite staging governor, zero ranges; killed-daemon `fio --verify`, both torn tails, multiqueue FLUSH negative control, stalled-daemon handling, and slow-compactor isolation | G2, pending |
| Census and local comparisons | Dated synthetic fleet; fixed 4/16 KiB and aligned CDC; allocation and clone accounting; R0/R1/R3 tables with swept capacity, memory, latency, amplification and spread | G3, pending |
| Two hosts | Static rendezvous ownership; GET/HAS/PUT/LIVE/JOURNAL; durable owner acknowledgments, pins and repair, offline sweep, both k modes, fleet class, fenced disk handoff | G4, pending |
| Read and FLUSH costs | Daemon TCP, nvmet TCP/RDMA probes, null/file targets, cache/media states, quiet/bulk-load states, polling/blocking, prefetch and boot storm; record RDMA counters | G5, pending |
| Reproduction and report | Commands to rebuild the dated fleet and regenerate every table; archived configurations, raw data and analysis | G6, pending |

The census begins alongside R0 rather than waiting for the full daemon. Phase 0
is the specification's `zdb -S` two-clone control on an expendable test pool.
Its interpretation must be verified before citing its output.

For the census, implement immutable raw-image fixtures and exact fixed-size
counts first. Add guest allocation maps, base-in-place sharing, aligned sharing
elsewhere, and shifted-only duplicates as explicit columns. QEMU's image map
describes image/backing allocation; it does not report the guest filesystem's
allocation map. Do not substitute one for the other.
[QEMU image map](https://www.qemu.org/docs/master/tools/qemu-img.html#cmdoption-qemu-img-arg-map)

The FastCDC arm needs a recorded definition of snapping direction, restart
position after snapping, terminal chunk handling, normalization, and seed.
Freeze that adapter before census comparisons. Report its observed mean;
16 KiB is the algorithm parameter. FastCDC's rolling fingerprint is not the
BLAKE3 content ID. [FastCDC API](https://docs.rs/fastcdc/latest/fastcdc/v2020/index.html)

Every protocol message needs bounded decoding, retry/idempotency tests, and
durability evidence before a HAS hit or PUT acknowledgment can release a pin.
GET and JOURNAL have separate connections from bulk PUT. Fleet-class degradation
to local class must be explicit in logs and measurements. Migration must fence
both root records before the new writer accepts writes. GC runs with writers
quiesced, consistent with concurrent GC being outside the study.

The two-host reservation and dedicated test device have not been established in
this iteration. Development continues on Spark; host formatting, testbed setup,
and measured runs wait for identified experiment resources. Keep the existing
descoping order and the week-2 threshold freeze from the specification.
