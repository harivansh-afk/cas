# Peer chunk reads

`cas-host` can serve and fetch immutable 4 KiB chunks by BLAKE3 hash over
kernel TCP. This is a static two-host read experiment, with local durability.
The optional [fleet layer](fleet.md) adds placement, chunk replication,
provisioning, fenced migration and coordinated collection. Authentication and
fleet-class journal durability are not implemented.

## Configuration and experiment setup

The service is disabled unless configured. The serving options are:

| Option | Meaning |
| --- | --- |
| `--peer-listen IP:PORT` | Bind the chunk service; use loopback or a private testbed interface |
| `--peer-address IP:PORT` | The other host, contacted on a settled-chunk local index miss |
| `--peer-force-remote` | Bypass the local settled chunk store and, by default, its cache |
| `--peer-cache` | Admit verified remote fills into the existing bounded clean cache |
| `--peer-inline-us N` | Additional reactor-local polling window (0–1000 µs, default 0) |

Force-remote and caching require a peer address. There is no implicit discovery,
address resolution, self-fetch, or forwarding. Both hosts may have a listener
and name each other. GET service always reads its own cache/store. Cache fills
are memory-only and never advance any durable prefix.

**Experiment setup uses force-remote on replicas**, corresponding to the
partitioned-mode descoping choice. Prepare the same disk contents on both hosts
through the existing guest/harness write path, FLUSH, and wait for compaction
through the written prefix. Verify `D = E = P` before measurement. Retain the
local copy on A; restart A with `--peer-force-remote` and leave `--peer-cache`
off. This disables A's local copy for settled reads without changing the disk
format or weakening recovery checks. A missing local-index entry also triggers
GET at runtime, while a cold-open manifest still requires every supposedly local durable
dependency. The fleet layer additionally accepts persisted owner receipts for
remote k=1 dependencies.

With existing prepared roots, catalog image ID `02020202020202020202020202020202`,
store ID `01010101010101010101010101010101`, and fresh report/socket directories
on a different filesystem, append the peer options to the normal host command:

```sh
# Host B (replace the addresses with the private CloudLab interfaces).
cas-host --root /mnt/cas --store 01010101010101010101010101010101 \
  --mode cold --image 02020202020202020202020202020202=/run/user/1000/cas-b.sock \
  --reports /run/user/1000/cas-b-report --telemetry \
  --peer-listen 192.0.2.2:17483 --peer-address 192.0.2.1:17483

# Host A: same prepared contents; settled guest reads must cross TCP.
cas-host --root /mnt/cas --store 01010101010101010101010101010101 \
  --mode cold --image 02020202020202020202020202020202=/run/user/1000/cas-a.sock \
  --reports /run/user/1000/cas-a-report --telemetry \
  --peer-listen 192.0.2.1:17483 --peer-address 192.0.2.2:17483 --peer-force-remote
```

Use the root's actual geometry and IDs. These example addresses are placeholders.
On one machine, use distinct roots/sockets and loopback ports, e.g. 17483/17484.
The endpoints have no authentication or encryption; bind only on the isolated
experiment network. The implementation verifies content integrity, not identity
or authorization. See [test fixtures](testbed.md) for storage and guest setup.

Read the measured working set once through A to warm B's chunk cache; size B's
`--cache-bytes` to hold it. Confirm B's `peer.counters.server_memory` increases
without `server_store` increases during the measured interval. A's verified
GET count should increase and its cache should remain empty when caching is off.
Guest direct IO or guest-cache control is still necessary to reach the daemon.
For the local NVMe comparison, disable force-remote and control both CAS cache
and device/page-cache state; this slice does not itself establish that control.

## Wire protocol

All integers are little-endian. Each frame starts with this 24-byte header.
The prefix includes the entire header; the maximum frame is 4,120 bytes.

| Offset | Bytes | Field |
| --- | --- | --- |
| 0 | 4 | Total frame bytes |
| 4 | 4 | `CASP` magic |
| 8 | 1 | Version, 1 |
| 9 | 1 | GET = 1, HAS = 2 |
| 10 | 1 | OK = 0, MISSING = 1, FAILED = 2; requests require OK |
| 11 | 1 | Reserved, zero |
| 12 | 8 | Nonzero request ID, echoed by the response |
| 20 | 4 | Hash count, echoed by the response |

GET carries one 32-byte hash. OK carries exactly 4,096 bytes; MISSING and FAILED
carry no body. Every received GET payload is BLAKE3-checked against the requested
hash before it can enter a cache, a shared fetch cell, or a guest response.
Malformed lengths, identity/version/opcode mismatches, EOF, deadlines, and bad
hashes complete with an IO error. Error payloads are never copied to the reader.

HAS carries 1–128 hashes and returns `ceil(count / 8)` bytes. Bit `i % 8` of
byte `i / 8` is **one when hash i is missing** from fenced local storage, matching
the design's missing-bitmap convention. Padding bits are zero. Cache-only copies
are not advertised as durable. FAILED carries no bitmap. `cas_daemon::peer::has`
is the bounded provisioning probe API.

The first frame fixes a connection's operation; mixing HAS and GET closes it.
GET has its own persistent connection, with multiple outstanding requests and
ordered responses; it is independent of HAS connections. Both ends set
`TCP_NODELAY`. The reactor sends GET directly on an established nonblocking connection and
consumes immediately available replies. With `--peer-inline-us N`, it additionally
polls for at most N microseconds before yielding transport ownership. A reply
completed there bypasses the eventfd/io_uring completion handoff. Slow replies
use one TCP worker with `poll` for readiness and partial send/receive handling.
Connection establishment stays on that worker, outside the shared mutex. This minimal implementation uses
blocking service workers; it is not the spec's proposed io_uring socket driver.
Bounded inline polling is an experimental arm, not continuous busy polling. The image reactor waits on completion eventfds via io_uring,
so a blocked peer does not block WAL IO or the reactor thread.

The client preallocates 1,024 slots, each with a 4 KiB payload and eventfd, using
the host metadata budget. Requests, reply assembly, and pending queues reuse
fixed storage; no request allocation is added by the transport hot path. Existing
admission, read-plan, and coalescing allocations remain. Four service workers
reuse aligned header/payload scratch. A hot GET serves a pinned clean-cache
entry without allocating; a store miss can allocate a normal bounded cache fill.
Disk-backed service reads participate as demand sources in the existing IO
scheduler, retaining its reserved background opportunities. The four-connection
cap is shared by GET and HAS; this slice offers no bulk traffic priority proof.

A GET expires two seconds after entering its slot, including connection setup.
Failed connections are discarded; subsequent requests can reconnect. A partial
server frame has a two-second absolute deadline. Idle connections remain open;
shutdown interrupts idle waits. Cancelled readers retain their slots until the
worker finishes, preventing reuse of a buffer still owned by transport IO.
The embedding host's shutdown remains nonblocking while workers drain.

## Read path and failure semantics

`local/reactor/read.rs` still resolves pinned WAL ranges first, then manifest
holes/hashes, cache, shared fetch registry, and local store. Only an absent store
plan (or force-remote) selects the new remote stage. Hash misses coalesce through
the existing registry; the original read credits remain held through the final
shared reader. The reply is verified before publication, optionally cached,
then copied into the admitted read buffer. Neither compaction publication nor
WAL reclamation, mutation order, admission limits, FLUSH, or recovery validation
is relaxed. Newer staged writes and zeros override the remote manifest entry.

Peer errors follow the existing fail-closed shared-read path: the guest receives
`VIRTIO_BLK_S_IOERR`, the image/host gate fails, and subsequent serving requires
recovery/restart. There is no automatic local fallback in force-remote mode and
no promise that an interrupted guest session recovers merely when a peer returns.
The transport itself can reconnect, as checked independently of the host gate.

## Instrumentation and local validation

When peer support is configured, `Host::report()` includes `peer`, also present
in normal host reports and telemetry. Counters cover GETs, verification/errors,
connections, peak outstanding GETs, server memory/store hits, missing chunks,
HAS, direct/inline attempts and completions, and completed/failed remote reads.
An inline attempt includes the default single nonblocking visit; it does not
necessarily indicate that a busy-poll window was enabled. No JSON formatting or file writes occur
on the IO path.

A fixed ring retains the most recent 1,024 remote-read records. Nanosecond values
use one monotonic host-local origin; clocks across hosts are not compared:

- `received_ns`: descriptor discovery at the guest frontend, or submission at
  the native harness (before admission attempts).
- `phases.send_ns`: client begins sending GET, including any worker queue delay
  before this point; connection establishment precedes this milestone.
- `phases.first_byte_ns`: first successful userspace receive of the reply.
- `phases.verified_ns`: complete payload has passed BLAKE3 verification.
- `completed_ns`: guest status/used publication, or native harness receipt.

`success` marks completion and `chunks` counts remote chunks for the request.
Zero milestones mean an unreached phase on failure. Multi-chunk reads retain
first send/first byte and last verification; coalesced waiters inherit their
leader's transport times, which can precede the waiter's receipt. Cancelled
observations record failure when released. The ring overwrites old records;
telemetry snapshots are samples, not a lossless long-run trace. The existing
optional read attribution remains separate.

```sh
just check
CAS_PEER_EVIDENCE="$PWD/results/peer-fetch/release-loopback" \
  cargo test --release -p cas-daemon --lib \
  local::host::tests::peer::two_daemon_loopback -- --nocapture
```

The integration test launches **two separate processes** embedding the actual
Host, peer service, admission, reactor, and native read harness. Both prepare
64 distinct chunks, A forces remote with caching off, and every read is checked
against its expected bytes. It performs 100 warmup plus 600 measured QD1 reads,
four pipelined reads (within existing per-image read credits), then stops B and
requires an error without published bytes. It retains reader, pipeline, server,
and failure JSON reports when `CAS_PEER_EVIDENCE` is an absolute output path.
The ignored child test is an entry point invoked by the parent, not skipped
integration coverage. No QEMU/KVM or dedicated allocation filesystem is needed
for this native fixture; aligned O_DIRECT storage is still required.

See [validation](validation.md) for the exact tested revisions, development
latencies, failures/retries, and limits. CloudLab still needs matched builds,
private NIC binding/firewall configuration, prepared replicas, guest IO,
cache-state verification, local NVMe controls, recorded NIC/IRQ/kernel settings,
transport baselines, repeated runs, and cross-host latency/throughput/CPU data.
The fleet layer supplies placement and bulk PUT; their effect on GET tails still
needs a measured contention experiment. Prefetch and fleet-class FLUSH remain open.
Compare polling settings under controlled CPU affinity; shared-host contention
can make bounded polling slower than the default path. Local correctness is not the paper's performance
result.
