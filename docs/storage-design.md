# Local storage architecture

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
Independent reads can bypass capacity-blocked writes within bounded descriptor
metadata; overlapping operations and FLUSH barriers still constrain ordering.
This is not a bound on worst-case read latency.

## Compaction and collection

Compaction processes durable WAL data, hashes fixed 4 KiB chunks, and writes
missing content into shared chunk segments. Chunk data is synchronized before
publishing a copy-on-write manifest commit. Covered WAL payload is reclaimable
only after relevant readers and retained replay identities release it.

Snapshots capture manifest roots; clones have private manifests and share chunk
content. Collection accounts for retained roots and readers. Physical space
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

The implementation is single-host, with one writable attachment per image.
It has no remote-read service, replicated durability, ownership transfer, or
migration protocol. The CLI lab fixes membership at creation. Native tests and
KVM crash fixtures are not physical power-loss tests or production readiness.
See [current work](../TODO.md) and [validation](validation.md) for open acceptance.
