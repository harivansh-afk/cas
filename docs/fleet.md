# Static fleet membership

The optional fleet layer adds rendezvous placement, durable chunk PUT, manifest
provisioning, fenced image migration, missing-chunk synchronization and a
coordinated sweep. FLUSH remains **local class**: it waits for this host's WAL
fdatasync, never silently substitutes peer durability. No JOURNAL or fleet-class
FLUSH is implemented. GET and its timing ring are described in
[peer reads](peer-fetch.md).

## Configuration and operation

All roots use the same store ID and segment geometry, distinct initial image
IDs, and stable member IDs 0 through N−1. Set the same `--peer-k` everywhere:
1 partitions chunks, N replicates them on every member, and intermediate k selects
that many owners. The persisted membership records node, N and k; a different
configuration or omission of fleet configuration fails reopening. Bulk connections
verify the target member ID, N, k and store ID. Static IDs must never be reassigned
to an empty replacement root. Membership reconfiguration is not implemented.

Supply ordered GET and bulk lists, including this host, in the same order on every
member. For example, node 0 in a four-member k=2 fleet:

```sh
--peer-node 0 --peer-k 2 \
--peer-listen 10.0.0.1:17483 --peer-bulk-listen 10.0.0.1:17485 \
--peer-get-member 10.0.0.1:17483 --peer-bulk-member 10.0.0.1:17485 \
--peer-get-member 10.0.0.2:17483 --peer-bulk-member 10.0.0.2:17485 \
--peer-get-member 10.0.0.3:17483 --peer-bulk-member 10.0.0.3:17485 \
--peer-get-member 10.0.0.4:17483 --peer-bulk-member 10.0.0.4:17485
```

Other hosts change only node and their local listeners. The legacy single
`--peer-address` / `--peer-bulk-address` pair remains a two-member shorthand;
it cannot be mixed with member lists. All members must run the same protocol
revision. These unauthenticated services require an isolated experiment network.
Each production root needs an exclusive allocation filesystem; sockets, reports
and telemetry live elsewhere.

In fleet **cold** mode `cas-host` stays alive as a peer server even without
`--image`. Its explicitly configured `--image ID=SOCKET` entries may name future
imports; the socket starts only when that image's complete durable graph and
writer root permit activation. Existing initial images first claim their ID on
every member; conflicting claims fail closed. Fleet cold mode permits a subset of
the catalog as guest endpoints. Retained recovery keeps its original requirement
for the entire catalog and is not a migration/reconnect protocol. A socket serves
one attachment; reconnecting it requires the existing cold/retained restart
procedure. Use a process supervisor to stop persistent fleet members; collect
live counters before stopping with `cas-fleet report` or rotating telemetry.

Commands address **bulk/control** ports:

```sh
# Copy the settled manifest into a new guest identity on node 1.
cas-fleet provision --source 127.0.0.1:17485 --destination 127.0.0.1:17486 \
  --image 02020202020202020202020202020202 --target 04040404040404040404040404040404

# Move the original image, preserving its identity and advancing writer generation.
cas-fleet migrate --source 127.0.0.1:17485 --destination 127.0.0.1:17486 \
  --image 02020202020202020202020202020202

cas-fleet sync --address 127.0.0.1:17485
cas-fleet sync --address 127.0.0.1:17486
cas-fleet sweep --member 127.0.0.1:17485 --member 127.0.0.1:17486
# For N members, repeat --member for every bulk endpoint in member-ID order.
cas-fleet report --address 127.0.0.1:17485
```

Provisioning requires chunks already durable at the selected owners; it verifies
this with HAS and never substitutes a payload copy. It exports the selected
manifest file prefix, including its historical pages, plus a 24-byte empty-tail
header. The new image starts a private sequence namespace at D=E=P=0. This is a
real remote-manifest experiment setup: provision, sweep k=1 surplus, disable
reader cache fills, warm the owner, and observe GETs on local index misses.
The earlier force-remote replica setup remains available as a separate control.

Migration quiesces host admission, drains and FLUSHes attached devices, captures
E, and sends the selected manifest and logical WRITE/ZERO mutations in (D,E].
It does not copy image payload below D. The receiver uses its own segment tickets
and attachment epoch while preserving mutation sequence numbers. Generation g+1
is durably fenced on the source before the destination can accept it. Every
other member records a non-writable witness after checking that exact source
fence. The transfer metadata identifies its sender, so migration can pass through
any sequence of members and return after several generations. The
receiver checks that exact source record, persists Receiving, verifies complete
file digests and dependencies, fsyncs the imported graph, publishes the catalog,
checks the source fence again, then persists Active. Only Active for the local
member permits attachment. No timeout rolls ownership back. Failure after a fence
may require restarting the failed member and retrying the same migration; it
never grants the old writer permission to resume.

Returning migration is supported after detaching the previous source attachment.
The older fenced catalog entry is retired before installing the next generation.
Interrupted, unpublished output is retained under an unreferenced image directory;
its segment names remain visible to the monotonic allocator. Cold recovery treats
the root's recorded E as mandatory evidence and rejects a shorter WAL before
repair. Tests exercise these transitions with real process kills.

The operation moves the block backend state, not QEMU CPU/RAM/device state.
The [two-host runner](two-host-experiments.md) orchestrates clean guest shutdown,
fenced handoff, cold boot on B, data verification and workload continuation.
It measures total guest downtime separately from daemon timings. Quiescence currently pauses the whole host. Fenced source manifests,
old WALs and rejected import artifacts are conservatively retained; they count
against physical capacity. Capacity experiments should use fresh roots and report
these retained recovery bytes separately from live chunk/index occupancy.

## Placement, shipping and collection

For each chunk, score every member with BLAKE3 over
`"CAS rendezvous v1\0" || hash[32] || member_u8`, compare digest bytes
lexicographically, and break ties with member ID. The top k members own the chunk.

Compaction preserves the existing local durable-store/manifest fence and queues
hashes after publication. A bounded shipper batches up to 63 chunks, queries HAS,
and sends only missing owner content. Each receiver verifies every chunk and the
complete segment before insertion, synchronizes the store, then acknowledges.
An interrupted/ambiguous PUT is retried with HAS. Local surplus is retained until
sweep confirms every selected owner's durable copy; peer outages therefore do not block
local-class compaction or FLUSH while local capacity remains. A full queue or a
restart triggers a budgeted inventory rescan. Explicit sync uses the same shipper.
GET routes to a nonlocal rendezvous owner through a preallocated connection per
member. A transport failure surfaces as guest IO error; automatic replica
failover is not implemented. Idle EOF is observed before reusing a connection.
GET has a separate listener/socket/worker pool from PUT and control traffic;
background disk work uses the existing IO scheduler. Compaction and shipping
share its background class: each waiting caller retains its own pending grant,
and their combined share remains one opportunity per three ready demand
submissions. An idle class lends its opportunities to the other.

Sweep freezes every member with durable epoch records, drains compaction and
shipping, verifies each complete sorted live-hash set with counts and digests, distributes
their common union, and
marks active and retained manifest/snapshot roots. Owners retain referenced
chunks; nonowners retain surplus if the owner lacks it. Remote-dependency
receipts are durable before a local surplus copy is removed. Store collection
copies live contents of mixed segments to durable outputs before deleting the
originals. Each host checks that every member reached the same durable swept state before
release. A crash or lost coordinator leaves its epoch frozen, including after
restart; rerun the same `sweep` command to finish it. There is no unilateral thaw
or collection while a peer is unreachable. Persisted dependency receipts allow
cold-open k=1 manifests without pretending absent local-owner chunks are healthy.

The implementation preallocates 64 image lifecycle slots per running fleet host;
return migrations consume slots until restart. Bulk buffers and metadata use
existing budgets. Incoming staging reserves its conservative WAL bound before
output. The runtime allocation audit remains open; this feature is not a claim
that every pre-existing allocation is accounted.

## Bulk framing and measurements

The GET protocol is unchanged. Bulk/control uses a separate TCP connection with
TCP_NODELAY and a bounded blocking exchange. Its 24-byte little-endian header is:

| Offset | Bytes | Meaning |
| --- | --- | --- |
| 0 | 4 | Total frame length, at most 24 + 1 MiB |
| 4 | 4 | `CASB` |
| 8 | 1 | Version 1 |
| 9 | 1 | Opcode |
| 10 | 1 | Flow: ship, provision, migration, sync, collection, control (0–5) |
| 11 | 1 | Reply status: success 0, error 1, retryable busy 2; requests use 0 |
| 12 | 8 | Nonzero connection-local request ID |
| 20 | 4 | Reserved zero |

HAS (1) carries up to 63 hashes and returns a missing bitmap (set = absent).
PUT (2) carries a u32 count, `hash[32] || bytes[4096]` records, and a trailing
BLAKE3 over count and records. It is one bounded logical segment, repacked into
the receiver's on-disk framing and ticket namespace. STATUS/ROOT/CLAIM/SYNC/report
use opcodes 3–7; freeze/LIVE/read/sweep/release use 10–14; image export/begin/bytes/
finish use 20/23/24/25. Image metadata uses bounded JSON; file fragments carry the
metadata digest, part, offset and up to 64 KiB. WITNESS uses opcode 8; STATUS
byte 4 carries N. N+2 bulk workers each reuse bounded request/reply buffers; operations have 30-second transfer deadlines.
Transport is kernel TCP with standard sockets; it is not an io_uring TCP engine.

`Host::report().fleet` and `cas-fleet report` expose cumulative actual application
bytes sent/received per flow, including framing, checks, ACKs, retries and partial
bodies. They exclude Ethernet/IP/TCP headers and retransmissions. GET client bytes
are in `peer.get_wire`. Controller transfer/sync calls use the separate `control`
flow; inter-daemon file traffic uses `provision`/`migration`. Sum sent bytes over all members
for inter-daemon traffic, not sent+received on both ends. Collection currently
relays LIVE sets through its coordinator, so its counters include that traffic.
Take before/after snapshots and keep background `ship` separate from explicit
`sync`; background repair may already have removed differences before a sync.

`fleet.shipping` reports the durability window of local-first writes. For each
acknowledged PUT the shipper records `queued_ns` (when the batch's first hash
was queued after compaction publication), `acked_ns` (when the owner's durable
acknowledgement arrived), `chunks`, `bytes` (PUT body bytes) and `owner`
(member ID). The ring keeps the last 1,024 batches (`retention`, `next_slot`);
`batches`, `chunks` and `bytes` are cumulative, and `window_batches` with
`window_ns_sum` cover the batches whose queue time is known, so the ring gives
the distribution and the sums its mean. Batches assembled from an inventory
rescan (startup, a full queue, retry after a failed pass) have a null
`queued_ns` and count in `rescan_batches` instead. A batch whose owner already
held every chunk sends no PUT and is not recorded. Nanoseconds are read from
the peer metrics clock, the same monotonic host-local clock as `peer.recent`;
never compare them across hosts. The
acknowledgement means the owner's `Store::insert` and store synchronization
completed, not fleet-class FLUSH.

The migration reply includes manifest/tail byte counts and additive monotonic
microsecond intervals: freeze (admission drain/FLUSH), prepare (tail encoding,
manifest hashing and owner readiness), fence (source fence and destination
Receiving exchange), transfer (file fragments), install (receiver durable graph
and Active publication), and finalize (source attachment fencing). Their sum is
`total_micros`. No interval claims QEMU/guest resume latency. See
[validation](validation.md) for development observations and exact commands.

Tests cover N=2 and N=4 with k=1 and k=2, data-verified provisioning/migration/return,
independently drifted clones, duplicate sync, PUT interruption, handoff kills,
required-prefix rejection and interrupted sweep. They run independent daemon
processes using the native admission/reactor harness. CloudLab still needs real
100 GbE, dedicated storage, controlled CPU/IRQ affinity, hardware runs of the
QEMU/workload/capacity harness and physical host-loss validation. Local k=2 replication
alone does not protect a host's not-yet-shipped WAL tail.
