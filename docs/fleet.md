# Static two-host fleet

The optional fleet layer adds rendezvous placement, durable chunk PUT, manifest
provisioning, fenced image migration, missing-chunk synchronization and a
coordinated sweep. FLUSH remains **local class**: it waits for this host's WAL
fdatasync, never silently substitutes peer durability. No JOURNAL or fleet-class
FLUSH is implemented. GET and its timing ring are described in
[peer reads](peer-fetch.md).

## Configuration and operation

Both roots use the same store ID and segment geometry, distinct initial image
IDs, and stable member IDs 0 and 1. Set the same `--peer-k` on both: 1 partitions
chunks, 2 replicates them. Membership is persisted; reopening with a different
node/k, or without fleet configuration, fails. Bulk clients check the other
member's node, k and store ID. There is no membership reconfiguration service.

Add these options to `cas-host` on each host (node 0 shown):

```sh
--peer-node 0 --peer-k 1 \
--peer-listen 127.0.0.1:17483 --peer-address 127.0.0.1:17484 \
--peer-bulk-listen 127.0.0.1:17485 --peer-bulk-address 127.0.0.1:17486
```

Node 1 reverses the address pairs and uses `--peer-node 1`. Across machines,
replace loopback with the private NIC addresses. These unauthenticated services
are intended for an isolated experiment network. Existing filesystem requirements
still apply: each production root needs an exclusive allocation filesystem;
sockets, reports and telemetry live elsewhere.

In fleet **cold** mode `cas-host` stays alive as a peer server even without
`--image`. Its explicitly configured `--image ID=SOCKET` entries may name future
imports; the socket starts only when that image's complete durable graph and
writer root permit activation. Existing initial images first claim their ID on
both hosts; conflicting claims fail closed. Fleet cold mode permits a subset of
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
cas-fleet sweep --node0 127.0.0.1:17485 --node1 127.0.0.1:17486
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
is durably fenced on the source before the destination can accept it. The
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
Guest freeze, QEMU migration/reconnection and resume orchestration remain testbed
work. Reported freeze/fence/transfer/install times are daemon timings, not total
VM blackout. Quiescence currently pauses the whole host. Fenced source manifests,
old WALs and rejected import artifacts are conservatively retained; they count
against physical capacity. Capacity experiments should use fresh roots and report
these retained recovery bytes separately from live chunk/index occupancy.

## Placement, shipping and collection

For each chunk, score both members with BLAKE3 over
`"CAS rendezvous v1\0" || hash[32] || member_u8`, compare digest bytes
lexicographically, and break ties with member ID. The top k members own the chunk.

Compaction preserves the existing local durable-store/manifest fence and queues
hashes after publication. A bounded shipper batches up to 63 chunks, queries HAS,
and sends only missing owner content. Each receiver verifies every chunk and the
complete segment before insertion, synchronizes the store, then acknowledges.
An interrupted/ambiguous PUT is retried with HAS. Local surplus is retained until
sweep confirms the owner's durable copy; peer outages therefore do not block
local-class compaction or FLUSH while local capacity remains. A full queue or a
restart triggers a budgeted inventory rescan. Explicit sync uses the same shipper.
GET has a separate listener/socket/worker pool from PUT and control traffic;
background disk work uses the existing IO scheduler.

Sweep freezes both hosts with durable epoch records, drains compaction and
shipping, exchanges complete sorted live-hash sets with counts and digests, and
marks active and retained manifest/snapshot roots. Owners retain referenced
chunks; nonowners retain surplus if the owner lacks it. Remote-dependency
receipts are durable before a local surplus copy is removed. Store collection
copies live contents of mixed segments to durable outputs before deleting the
originals. Each host checks that both reached the durable swept state before
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
metadata digest, part, offset and up to 64 KiB. Four bulk workers each reuse
bounded request/reply buffers; operations have 30-second transfer deadlines.
Transport is kernel TCP with standard sockets; it is not an io_uring TCP engine.

`Host::report().fleet` and `cas-fleet report` expose cumulative actual application
bytes sent/received per flow, including framing, checks, ACKs, retries and partial
bodies. They exclude Ethernet/IP/TCP headers and retransmissions. GET client bytes
are in `peer.get_wire`. Controller transfer/sync calls use the separate `control`
flow; inter-daemon file traffic uses `provision`/`migration`. Count A.sent+B.sent
for inter-daemon traffic, not sent+received on both ends. Collection currently
relays LIVE sets through its coordinator, so its counters include that traffic.
Take before/after snapshots and keep background `ship` separate from explicit
`sync`; background repair may already have removed differences before a sync.

The migration reply includes manifest/tail byte counts and additive monotonic
microsecond intervals: freeze (admission drain/FLUSH), prepare (tail encoding,
manifest hashing and owner readiness), fence (source fence and destination
Receiving exchange), transfer (file fragments), install (receiver durable graph
and Active publication), and finalize (source attachment fencing). Their sum is
`total_micros`. No interval claims QEMU/guest resume latency. See
[validation](validation.md) for development observations and exact commands.

Tests cover both placement modes, data-verified provisioning/migration/return,
independently drifted clones, duplicate sync, PUT interruption, handoff kills,
required-prefix rejection and interrupted sweep. They run independent daemon
processes using the native admission/reactor harness. CloudLab still needs real
100 GbE, dedicated storage, controlled CPU/IRQ affinity, QEMU orchestration,
workload/capacity runs and physical host-loss validation. Local k=2 replication
alone does not protect a host's not-yet-shipped WAL tail.
