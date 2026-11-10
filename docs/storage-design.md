# Storage architecture

CAS serves multiple private block images from one Linux host. Stock QEMU
connects through vhost-user Unix sockets. The guest sees a virtio-blk device;
content addressing stays below that interface.

## Ownership

- `cas-core` owns formats, indexes, manifests, chunk storage, cache primitives,
  and memory/disk accounting.
- `cas-host` initializes and opens the catalog, supervises image sockets, and
  coordinates shared administrative operations.
- Each image has a frontend and IO reactor. The frontend retains descriptors,
  admits requests, and completes them to the guest. The reactor owns ordered
  mutations and io_uring storage work.
- A shared compactor writes the chunk store and publishes manifest changes.
  Shared budgets cover append, fetch, cache, and metadata allocations.

The store contains a catalog, shared chunk segments, per-image manifests and
write-ahead logs, and retained snapshot/recovery state. Persistent headers and
records use checksums and checked identities. A checksum detects corruption;
it is not authentication. [Storage encoding](storage-format.md) defines the
bytes; the parsers in `crates/cas/core/src/` enforce them.

## Writes, reads, and FLUSH

Writes enter aligned append buffers and are packed into WAL batches. Completed
mutations publish in image order. WRITE completion is not a durability promise:
FLUSH establishes the local durable boundary through `fdatasync`.

Three prefixes distinguish visibility from reclamation:

- **P**: the contiguous published mutation prefix.
- **E**: the prefix covered by a completed durability barrier.
- **D**: the prefix represented by the committed manifest and chunk store.

While serving, `D <= E <= P`. Reads resolve newer data from staging and older
data from the manifest/store. They retain source pins through completion.
Settled chunks missing from the local index can be fetched by hash from one
configured TCP peer. The receiver verifies BLAKE3 before publishing a read or
filling the clean cache. WAL reads remain local and take precedence.
[Peer reads](peer-fetch.md) documents framing, bounded resources, instrumentation,
and the force-remote experiment mode.
Independent reads can bypass capacity-blocked writes within bounded descriptor
metadata; overlapping operations and FLUSH barriers still constrain ordering.
This is not a bound on worst-case read latency.

## Compaction and collection

After a complete sweep reports capacity exhaustion, automatic collection waits
for a change in retained roots, attachment membership or physical allocation
and promises. Reading the same current root does not trigger another global
pause. Releasing the final old-root pin or advancing a manifest permits a retry;
explicit collection remains available. The worker checks delayed filesystem
frees without pausing readers. This prevents repeated no-progress sweeps but
does not remove the pause required by a productive collection.

A store fixes its chunk size at creation: 4 KiB by default, or the single-host
16 KiB arm (`cas-host init ... --chunk-bytes 16384`). Guest blocks, WAL alignment
and manifest addressing remain 4 KiB. Chunk segments and manifest FILE headers
record the geometry; mixing is rejected. Peer/fleet configurations reject the
16 KiB arm until their separate wire protocol supports it.

Compaction processes durable WAL data and writes missing fixed-size chunks into
shared chunk segments. For 16 KiB it coalesces affected aligned extents, resolves
each block from the last selected WAL edit or the pinned manifest at D, pads an
image-tail extent with zeros, then hashes the assembled chunk. It never reads a
newer mutable staging map: the selected durable WAL prefix bounds the new D.
Fully covered ZERO interiors remain range edits; only boundary/data chunks are
assembled. Preparation remains bounded (at most 442 mapping edits); the 4 KiB
arm retains its 318-edit input bound. Extra base-payload and manifest reads are
included in compaction read accounting. Chunk data is synchronized before
publishing a copy-on-write manifest commit. Covered WAL payload is reclaimable
only after relevant readers and retained replay identities release it.

Snapshots capture manifest roots; clones have private manifests and share chunk
content. Collection accounts for retained roots and readers. Fleet collection requires both hosts to quiesce and exchange complete live sets;
a standalone local sweep is refused on fleet stores. Physical space
reservations protect output/progress capacity rather than assuming logical
free space is immediately available to the filesystem.

## Live recovery

QEMU retains an inflight memfd across daemon replacement. CAS records request
identity, discovery/admission state, queue cursors, and publication state in a
bounded trailer. Recovery validates store/image/attachment identity and
reconciles completed and pending requests before serving resumes.
[Inflight encoding](inflight-format.md) defines this state. A format change can
require a fresh attachment; this is not general hot-upgrade compatibility.

Cold recovery inspects durable prefixes before repair. Rejected or incomplete
suffixes must not be interpreted as acknowledged work. Shutdown, reset, socket
failure, and host-wide failures have different ownership boundaries; their
regressions live beside the implementations and in `crates/harnesses/`.

## Limits

Durability and recovery remain local, with one writable attachment per image.
The optional [static two-host fleet](fleet.md) adds k=1/2 placement, chunk
replication, manifest provisioning, fenced migration and coordinated collection.
Its background replication does not strengthen local-class FLUSH or protect an
unshipped WAL tail from host loss. The CLI lab fixes image membership at creation;
fleet cold serving can wait for explicitly configured incoming image sockets. Native tests and
KVM crash fixtures are not physical power-loss tests or production readiness.
See [current work](../TODO.md) and [validation](validation.md) for open acceptance.


## Measuring chunk arms

Host reports and existing telemetry expose `store.index_memory`,
`store.compaction_read_bytes`, `store.store_bytes_appended`,
`store.collection_rewrite_bytes`, `cache.allocated_bytes` and
`metadata_cache.allocated_bytes`. Per-image status exposes `guest_bytes_written`
and `wal_bytes`. These cumulative counters restart with their owners: subtract
samples from the same process/owner, and do not interpret a rebuilt offline
index as the retained-capacity live index.

Guest bytes count published WRITE payload, excluding ZERO/DISCARD. WAL bytes
count successful published DATA, completed FENCEs and newly created segment
headers, excluding existing records scanned during recovery. Store bytes count successful batch and
segment-header writes, including collection; collection rewrite bytes are a
subset and must not be added twice. Compaction reads count completed loads and
preparations, including WAL headers/payload, base chunk headers/payload and COW
manifest reads, even if a later reservation defers publication. Failed attempts
may have partial IO and must not be treated as successful measurement runs.

These counters describe software traffic. Device write amplification still needs
NVMe/device counters and filesystem metadata/journal traffic. Report manifest
writes separately (`Manifest::bytes_written` in the core inspection fixture).
No software ratio here substitutes for the paper's device metric.

An offline command holds the store root lock, scans verified chunk records, and
prints rebuilt index memory without repairing files:

```
cargo run -p cas-core --example chunk-arm -- inspect /path/to/store
```

Live cache allocations and traffic counters are unavailable offline; the command
labels that scope explicitly. Use host telemetry for those measurements.

A deterministic development comparison creates two 1 MiB images per arm, writes
identical base data, then overwrites one 4 KiB block per 16 KiB extent in the
second image. It compacts, reclaims WAL, collects, verifies every guest block,
and prints stored payload, encoded/allocated chunk bytes, write/read traffic
and live index allocation:

```
cargo run -p cas-core --example chunk-arm -- fixture /path/to/new-output
```

Use fresh output for each run. This is development evidence, not a ZFS comparison
or a paper result: no dedicated-media repetitions, device counters, guest
latencies or equal-total-memory control are established by this fixture.
