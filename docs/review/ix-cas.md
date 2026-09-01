# ix CAS: what it is, what it measured, what broke, what it teaches

Review of Indexable's `ix` content-addressed store, written for the research
design (content-addressed block backend for VMs on stock QEMU, local staging
log with local fdatasync ack, background compactor into a chunk store, chunks
placed across two hosts by rendezvous hash, per-host RAM cache, cold remote
reads over TCP, migration by moving the offset-to-hash map, epoch GC).

Sources are the `ix` checkout at `/Users/rathi/Documents/Git/indexable/ix`.
Paths below are relative to that root. `deck` means
`docs/architecture/cas-storage-model.html`, a 19-slide model of the stack with
constants read from source and 30 days of hil production telemetry
(2026-07-04 to 2026-08-03). Every number here is either quoted from a doc or
doc comment, or marked as a code constant. Where ix itself says a value is
unmeasured, I say so.

Two of ix's own docs carry a warning worth repeating. The GC contract is
"reverse-engineered from the shipped code" because the planning documents it
cites never existed in the tree (`docs/architecture/cas-gc-contract.md:15-26`).
The write-path plan says every `1.6 ms` in it predates a deletion that changed
the number (`docs/architecture/cas-write-path-issue-stack.md:391-393`). Line
anchors drift; trust the code over the anchor.

## 1. What ix's CAS actually is

### 1.1 Data model

A chunk is an immutable byte string named by its BLAKE3-256 hash
(`crates/storage/cas/src/lib.rs:22`, `ix_hash::Content`). A `ChunkRef` is
`(offset u64, length u64, hash [u8;32])` (`cas/src/cas.rs:5-12`). Files and
volumes are manifests of `ChunkRef`s; the block-volume map is a prolly tree
whose pages are themselves BLAKE3-named CAS blobs
(`crates/storage/blockvol/src/seed.rs:12-20`). Everything reachable from a root
is in the same store, so one GC mark covers data and metadata.

The hard chunk cap is 63 MiB (`cas/src/lib.rs:53`). The reason is the wire, not
the disk. The QUIC fabric caps a request or response at 64 MiB, so a larger
chunk could be written locally but never fetched by a peer or pushed to its
owners, and would become "an un-transferable sole-copy chunk"
(`cas/src/lib.rs:31-51`). They made oversize a typed error at put time rather
than a silent strand.

### 1.2 Chunking

Content-defined chunking with AE (Asymmetric Extremum, Zhang et al. INFOCOM
2015), SIMD-accelerated per VectorCDC (FAST '25), hashless, with AVX-512, AVX2,
NEON and scalar backends (`crates/storage/cdc/src/lib.rs:1-16`). Defaults are
min 16 KiB, avg 64 KiB, max 256 KiB, a 1:4:16 ratio; the AE window is
`avg - 256` rounded to 16 (`cdc/src/lib.rs:39-93`). The doc comment says 64 KiB
"matches the sweet spot for source code trees", which is the file-system
workload, not blocks. For block volumes the spec proposes measuring 8/32/128
against 16/64/256 and marks that as open
(`docs/block-volumes/block-volumes-spec.md:246,314`, per the blockvol read).

Inputs at or below `min` skip CDC entirely and become one chunk
(`cas/src/chunker.rs:35-48`). The chunker only addresses; the caller stores.
`Chunker::chunk` returns the ordered refs plus a deduped set of chunks to store,
first occurrence wins (`cas/src/chunker.rs:23-33,88-107`).

The block-volume fold snaps every CDC boundary down to the 4 KiB block so no
extent straddles two chunks (`blockvol/src/fold/chunking.rs:1-21,98-112`).
Within a fold it dedups by hash but does no cross-fold presence probe
(`chunking.rs:85-91`).

### 1.3 Storage layout on a node

Chunk bytes live in packed append-only segment files, target 32 GiB each,
under `<root>/segments/<id/1024>/<id:010>.seg`
(`crates/storage/cas/disk/src/lib.rs:23-34`, `disk/src/segment/mod.rs:1-38`).
The size "matches SeaweedFS volumes and keeps a ~400 TB node at a few thousand
segments instead of the ~200M one-file-per-chunk inodes the V2 layout
produced". V2 was one file per chunk in a two-level hex directory tree; it
"made the leader disk-bound on cold ext4 inode lookups" and was replaced.

Record framing is `len:u32 | hash:[u8;32] | data | crc32c:u32`, and the CRC
covers only the `len || hash` prefix. The BLAKE3 hash verified on every read is
the data integrity check; the CRC exists for torn-tail detection on recovery
(`segment/mod.rs:22-38`). The hash is stored inline so recovery and compaction
can rebuild the index without re-hashing. A chunk location is
`(segment_id u32, offset u64, len u32)`, 16 bytes big-endian, and `offset`
points at `data`, so a read is one `pread` of exactly `len` bytes
(`segment/mod.rs:55-75`). Reads go through a 256-entry fd cache split 16 ways
(`segment/read_cache.rs:14-25`).

The index is LMDB, sharded 16 ways by placement group into independent
environments, each with its own single writer thread
(`disk/src/lib.rs:229-240`, `disk/src/catalog.rs`). The catalog is opened
`MDB_WRITEMAP | MDB_NOSYNC`, deliberately without `MDB_MAPASYNC` because that
combination "allows the OS to reorder mmap page flushes, which can corrupt the
B-tree structure on power failure" (`catalog.rs:19-35`). The catalog is not a
durability boundary; the stable fence is (section 1.7). A shadow in-RAM
hash table ("keydir", hashbrown keyed by the first 8 bytes of the BLAKE3) was
built to A/B the B-tree descent against a hash lookup; LMDB stays
authoritative and the keydir is a measurement (`catalog/keydir.rs:1-17`,
`docs/cas-drain/CAS-DRAIN-THROUGHPUT-HANDOFF.md:133-144`).

Per-shard LMDB writes go through a group committer: one thread, batch cap
4096 ops, queue depth 8192 (`catalog/group_commit.rs:19-27`). The 500 µs linger
it used to have was deleted after measuring 97% of closes unproductive
(section 3). Puts also pass a put coalescer: batch 1024, 500 µs linger, queue
2048, kept because it saves 98.2 to 98.4% of the commits it fronts
(`disk/src/put_coalesce.rs:11-16,33-58`).

All disk I/O runs on a dedicated bounded pool, not tokio's blocking pool.
Reasons given: admission control (tokio's pool is unbounded to 512 threads and
a cold-read flood oversubscribes the disk) and futex contention (one shared
condvar carried ~60% of futex traffic). The pool is split into 8 shard-groups
routed by hash, total permits `2 * nproc`, with strict-priority dequeue between
`Foreground` and `Background` and a reserved foreground floor of 2 permits per
group (`disk/src/diskpool.rs:1-95`). The class is a tokio task-local that does
not cross `tokio::spawn`; unscoped work defaults to Background
(`diskpool.rs:129-160`).

### 1.4 Placement

Deterministic CRUSH-style placement: `pg = fold64(hash) & (pg_num - 1)`, then
weighted rendezvous (straw2) from PG to a replica set
(`crates/storage/cas/placement/src/lib.rs:1-14`). Default `replica_count` is
2. Target about 100 PGs per node, sized for peak node count, power of two, and
"`pg_num` cannot change once data exists" (`placement/src/lib.rs:26-41`). The
leader holds no per-chunk index and is off the read and write path; leader
memory is O(nodes) (`cas/fabric/leader/src/lib.rs:1-14`). Placement is class
conditioned: a node has a weight per `ReplicationClass` (Steady, Bulk), so a
storage-heavy node can own bulk chunks without joining the steady owner sets
"whose stable fences gate guest-visible latency" (`placement/src/lib.rs:18-24`).

A related archived audit (`docs/_archive/design/agent-judgment-audit.html`,
case 1) records the one placement design fight in the record: whether the
per-PG catalog index should key by `pg || hash` (needs `pg_num` at write time)
or by `bitrev64(fold64(hash)) || hash` (a pure function of the hash). They
shipped the former. The cost, acknowledged in the PR body, is that "nodes that
start without a cluster map will stall writes until the heartbeat loop delivers
one", and a local-only node "parks every put forever" (ENG-2732). The audit's
own verdict is that neither side proposed the dominant hybrid: let presence
writes proceed unindexed and backfill when `pg_num` arrives.

### 1.5 Write path

From the deck (slides 3, 6, 10) and `docs/architecture/cas-write-path-issue-stack.md`:

```
guest write -> channel ring -> vmfsd -> volume staging -> commit gather
  -> prolly tree -> cas Put over /run/ix/cas-fabric.sock (foreground=true)
  -> put_coalesce (batch 1024, 500 us, FIFO)
  -> advertise-delete gate (16 shards, tokio::Mutex, FIFO)
  -> segment append (16 writers, pwritev)
  -> catalog group committer (16 LMDB envs, FIFO)
  -> disk pool (8 groups, strict priority)
  -> NVMe
  -> intent row staged for replication; drain runs later
```

The client declares its class three ways at the handle: `DiskClass`
(foreground/background), `ReplicationClass` (steady/bulk), and whether the
write owes replicas at all (`stage_replication`) (deck slide 4). The deck's
central finding is that this class is declared once and honored once. Three of
the four queues between the socket and the disk are FIFO and cannot read it
(slide 5).

Writes are name-before-write. A producer records the hash in a row a GC source
enumerates before uploading bytes, and the mark reaches it from that instant;
there is no reclaim horizon or grace timer
(`docs/architecture/cas-gc-contract.md:578-603`). The client type enforces this
with a typestate builder (`put -> promise -> presence_probe -> upload ->
finish_*`) and does not implement the generic `Store` write trait
(`cas/fabric/client/src/store.rs:34-41`).

Dedup on put is unanimous-owner presence. A hash counts as present for a skip
only if every current canonical owner answers present and not delete-pending;
a non-owner cache copy never satisfies a skip; an unreachable owner answers
`Unconfirmed`, which forces the upload
(`cas/fabric/server/src/leader_aware.rs:49-80`,
`cas-gc-contract.md:233-239`). The probe times out per owner at 10 s because an
owner that accepts the stream but never answers would park the local put
(`leader_aware.rs:49-59`).

A local put stages an outbound replica intent row per missing owner, in the
same LMDB commit as presence (`leader_aware.rs:1-9`). The intent table is the
durable work log for replication.

### 1.6 Read path

```
guest fault -> ring -> vmfsd -> volume read -> prolly tree -> cas Get
  -> local socket (foreground=true) -> keydir/LMDB lookup
  -> disk pool foreground -> pread
  -> miss? compute owners from cached cluster map, PeerGet{foreground:true}
  -> QUIC lane 7 -> peer disk pool foreground
  -> bounded whole-fleet fallback during map convergence
```

(deck slide 9; `leader_aware.rs:1-7`; probe fan-out capped at 8 peers,
`leader_aware.rs:39-44`.) The read path is the one the deck holds up as
correct: the whole local session is scoped foreground, the disk pool honors
it, and cross-node reads ride a dedicated QUIC endpoint so they never queue
behind a transfer frame. On the mapped local path there are zero copies:
segment fds pass over `SCM_RIGHTS` and the client mmaps directly (slide 9).
The one gap is that fd-passing forces one exchange at a time per connection
under a mutex, mitigated by dedicated read sessions per caller.

### 1.7 Durability

Segment appends are unsynced and the catalog is `NO_SYNC`. The only power-fail
boundary is the stable fence (`disk/src/stable.rs:1-20`). A fence promises that
every chunk acknowledged before it began is on stable media, "segment bytes
first, catalog reachability second". Fences coalesce into single-flight rounds
so the fdatasync rate is bounded by the round rate, not the barrier rate; a
fence arriving mid-round joins the next round and never trusts the running one.
Rounds run on their own 2-permit lane so a fence burst cannot pin a
shard-group's workers (`diskpool.rs:104-110`).

Three durability tiers are named in the W6.1 research
(`docs/research/vcfs-respine/W6.1-STABLE-TIER.md:9-19`): `accepted_local`,
`present_on_owners` (bytes in the owners' page cache), `stable_on_owners`. Only
the third releases a guest fsync barrier. The fan-out fence acks with
`AllOwners`; a quorum policy exists as an enum with one variant and is deferred
to issue #7747 pending SLO breach data (`cas/fabric/server/src/local.rs:40-53`).

Recovery honesty under `WRITEMAP`: the kernel may write catalog pages back
before segment bytes, so after a power cut the catalog can name bytes that
never hit media. The invariant they chose is "fenced hashes intact; unfenced
hashes may be absent but never served corrupt", and the unclean-shutdown read
path re-hashes anything not known fence-covered (`W6.1-STABLE-TIER.md:73-87`).

### 1.8 Replication drain and streaming

A chunk written locally owes replica copies. The legacy drain paged intents
(512 per page, 4 pages in flight, 32 concurrent peer pushes, 1 s tick, 10 s per
peer op, 24 MiB batch cap, process-wide 128 MiB/s pacer) and held one page slot
across `probe -> promise -> local read -> put -> peer stable fence -> retire`
for every peer in the page (`CAS-DRAIN-THROUGHPUT-HANDOFF.md:67-102`). That is
what made it slow (section 3).

The redesign (`docs/cas-drain/CAS-DRAIN-STREAMING-CONTRACT.md`) splits three
planes:

- Data plane. A long-lived per-peer transfer session anchored to one QUIC
  control stream; independent unordered 8 MiB frames on separate one-shot
  streams round-robined across six bulk-lane connections, under a
  receiver-granted byte credit window (sections 2 and 3).
- Durability plane. The receiver answers a frame with the exact hash list it
  now holds plus a horizon ticket. That is acceptance, not durability. A
  checkpoint every 100 ms or 256 MiB runs one stable-fence round and publishes
  a durable horizon (C-R-TICKET, C-R-CHECKPOINT). Tickets are post-increment so
  `H >= ticket` proves the certified commit preceded the snapshot; the doc
  spells out why pre-increment would be unsound (section 4).
- Retirement plane. The sender deletes an intent only when its ticket is
  covered by a published horizon on the same session and boot id, and after a
  local source fence (C-S-RETIRE). Any doubt retires nothing (C-S-SHORTFALL).

Two pieces of this are directly reusable. First, C-R-PIN: every ticketed hash
is pinned against eviction and delete-pending until a published horizon covers
it, because otherwise a reclaim between ticket and checkpoint could make a
deletion durable and still publish a horizon covering the ticket, and the
sender would retire the last copy. Second, C-R-BOOT: horizons, tickets, pins,
sessions and credit state are boot-scoped and never persisted; a receiver
restart voids all of them and the sender re-drives from its intent rows.

Lanes: 8 QUIC endpoints per peer, 1 control, 6 bulk, 1 foreground read. The six
bulk lanes are not QoS. Each is a separate UDP source port so LACP layer3+4
hashing spreads them across bond slaves; K=6 because K=4 left a slave underused.
One 5-tuple measured ~9-10 Gbit/s; eight endpoints 27.6-29.6 Gbit/s (deck slide
8; `docs/snapshot-design/11-replication-drain-classes.md:164-166`).

### 1.9 Credits and backpressure

Three separate mechanisms exist, and the deck argues they are compensating for
each other.

The disk pool's strict priority plus reserved floor (1.3) is structural and
works. The receiver credit window is computed from disk-pool headroom,
nondurable-backlog headroom, an in-flight memory budget, and a foreground
pressure signal; it is shared across all live sessions so N senders cannot
each be granted the full headroom (C-R-CREDIT). The pressure loop samples three
foreground-classed wait counters every 500 ms (disk-pool fg queue wait 50 µs/op,
advertise-gate wait 5 ms per fg batch put, group-commit wait 10 ms per fg
mutation) and collapses the background credit ceiling to zero within one
interval, then restores it in 4 steps over 2 s
(`cas/fabric/server/src/pressure.rs:1-100`). Only `TransferFrame` is credit
gated; `Get`, `PeerGet`, and direct puts are not (deck slide 16).

The deck's judgment (slide 18): I-FG-ZERO, foreground sees zero measurable
slowdown from drain, "is held by a control loop" rather than by structure, and
"converting I-FG-ZERO from compensator to structure is the single largest
simplification available in this stack".

Above the CAS, the block-volume write log has its own debt governor: pacing is
zero below W/2, ramps quadratically to the drain rate at W, and parks writers
at a hard ceiling of 2W; W = 6 GiB, ceiling 12 GiB, max pacing sleep 500 ms,
flush interval 50 ms, `T_idle` 5 s, `T_promote` 15 s (blockvol design/04,
`storage-log` constants, per the blockvol read). Backpressure never surfaces as
an errno because `VIRTIO_BLK_S_IOERR` shuts XFS down
(`blockvol/README.md:66-69`).

### 1.10 GC

Fleet GC (`docs/architecture/cas-gc-contract.md`) is a reachability mark, not
a refcount and not an age. "The delete discriminator is mark membership against
a single consistent Postgres snapshot, never a numeric clock" (section 1). The
`chunk_refs` table and its refcount surface were removed. A chunk class covered
by no registered liveness source is deleted, not merely unmarked, so source
coverage is "a data-loss surface" (section 1).

Mechanism:

1. S0 mark. One `REPEATABLE READ` transaction on the Postgres primary captures
   roots from every source, commits, then walks CAS. Safe to release the
   snapshot before the walk because CAS is immutable (section 3).
2. Quiesce. For each candidate, mint a `BarrierId = (leader_term, barrier_seq)`,
   send `SetDeletePending` to every current owner, await all acks. After this
   owners answer dedup-absent for the hash so a racing producer re-uploads
   instead of skipping. This closes the resurrection race (section 4).
3. S_final. One fresh full traversal after the last ack. "Never a shallow
   S0-candidate re-check or a changed-roots-only delta" (section 3).
4. Commit. Each owner deletes iff its durable delete-pending flag carries this
   exact barrier id. A put that re-creates a mid-delete chunk cancels the
   pending delete in the same LMDB commit (section 4).

Owner set and dedup probe must see the same placement map, so map swaps are a
two-phase `PrepareMap` / `ActivateMap` barrier and the leader re-affirms the
map before every quiesce batch (section 4, "placement-map barrier").

Every uncertainty resolves toward a leak: a source that cannot enumerate must
return `Err`, never a partial list; an offline owner defers; a leader crash
retains; a failed commit send is a leak (sections 4 and 5). The required source
set is stored as data in Postgres (`cas_gc_required_sources`) because a rolling
deploy can run an old leader whose compiled source list predates a new source,
and it "self-checks green and then builds a mark covering no chunk of the newer
source's class" (section 5).

The dial went `reclaim -> disabled` on 2026-07-26 and was `disabled` in every
region when the doc was written (section 2). At the hil backlog, ~500M dead
chunks region-wide at 5M per hourly tick was "about four days of ticks, each
paying a full ~20-minute O(live set) mark build", which is why a drain loop of
back-to-back batches per tick was added (section 6). The sweep once fetched
every candidate to re-verify absence, which "priced the sweep at O(dead bytes)
in fabric reads, a multi-day collect on a garbage-dominated region"; removed
(section 6).

Local space reclamation is separate. Deletes drop the catalog row and credit
dead bytes to the segment; compaction copies live chunks forward into the
active segment, commits the new locations, then unlinks the old file, never
mutating a segment in place (`disk/src/compaction.rs:1-60`). Eviction under
free-space pressure reclaims whole segments in tiers: surplus non-owner copies
at chunk granularity, then fully dead segments, then coldest live segments
whose every chunk is replicated elsewhere (`disk/src/eviction.rs:1-60`). Tier 0
exists because a fleet-wide class flip of 613,545 chunks produced zero dead
bytes and zero evictions: segments are packed in arrival order, so one
class-mixed segment holds both surplus and homed chunks and 16 of 16
candidates were kept (`eviction.rs:24-41`).

### 1.11 The block-volume analog

`crates/storage/blockvol` is the closest thing in ix to the research system.
Guest runs XFS on virtio-blk; the host backend is in-process in the VMM. Writes
append to a per-volume log and a RAM dirty index at 4 KiB granularity, ack from
page cache, and FLUSH fdatasyncs through a `fetch_max` LSN watermark. A fold
coalesces latest-per-block, CDC-chunks with block-snapped boundaries, puts to
CAS, and commits a prolly-tree extent map. Reads overlay the dirty index on the
committed tree and never consult the log. Snapshot is an LSN cut; migration
moves the head plus a shipped log tail. Its numbers are in section 2.8 and its
lessons are folded into section 4; the source docs are listed in section 5
under "Block volumes".

## 2. Numbers

All from hil production or dev boxes unless marked "code constant" or
"unmeasured". "hil" is a three-node region with 128-core nodes.

### 2.1 Chunk and object sizes

| Item | Value | Source |
|---|---|---|
| CDC min / avg / max | 16 / 64 / 256 KiB (code constant) | `cdc/src/lib.rs:39-45` |
| Hard chunk cap | 63 MiB, below the 64 MiB QUIC request cap | `cas/src/lib.rs:53` |
| Segment target | 32 GiB (code constant) | `disk/src/lib.rs:34` |
| Production drain bytes per intent | 24,318 to 151,051 B, peaks at 24 KiB and 34 KiB | `CAS-DRAIN-PERFORMANCE-DESIGN.md:315-319` |
| Seed ingest blob size | 69,104 B/blob | `snapshot-design/11:16-121` |
| Lazy restore object size | 231 KiB at 5.06 ms per object | deck slide 13 |
| Extent map bytes per dirty MiB | 161 B sequential, 6,182 B random 4 KiB | blockvol design/02:254-260 |

### 2.2 Local write path latencies

| Item | Value | Source |
|---|---|---|
| Advertise gate, idle | 3.3 µs mean | `pressure.rs`, deck slide 5 |
| Advertise gate, drain active | 19.3 ms mean | same |
| Advertise gate, pure guest PUT load, no drain | 3.0 to 39.8 ms | same (V0b) |
| Group commit, idle / drain active | 1.29 / 3.76 ms mean | same |
| Group-commit linger fill before deletion | 97.2-97.3% of ~186-241 batches/s waited the full 500 µs to fold in nothing; mean fill 1.03 ops; 0.553 ms paid to avoid 2.8% of commits | `cas-write-path-issue-stack.md:379-389` |
| Raw durable 4 KiB LMDB commit | 16.5 µs | same |
| Per-shard slice cost (gate held across append + linger + commit) | ~1.6 ms, now ~1.1 ms | `issue-stack.md:37-57` |
| Implied receiver ceiling from that | ~617 chunks/s per shard | same |
| Disk-pool fg queue wait, ambient | 0.08 to 0.31 µs per run | `pressure.rs:77` |
| Cold guest reads behind replication (pre-fix, ix#7901) | queued 100 to 400 ms | `diskpool.rs:67` |
| Disk pool permits on 128-core node | 256 across 8 groups, 32 each, 2 reserved fg | deck slide 6 |

### 2.3 fsync floor (BARRIER-FLOOR.md, 1000 samples each)

| Device | 4 KiB p50 / p99 | 64 KiB p50 / p99 / max | 1 MiB p50 / p99 / max |
|---|---|---|---|
| ext4 on mdraid NVMe (idle) | 0.060 / 0.162 ms | 0.085 / 0.180 / 0.347 ms | 0.381 / 0.506 / 1.074 ms |
| ZFS with mirrored SLOG (idle) | 0.635 / 0.944 ms | 0.331 / 1.437 / 15.218 ms | 1.494 / 3.123 / 23.053 ms |

Separate PLP NVMe RAID1 (W9:27-29): fdatasync avg 35 µs, p99 78 µs, p99.9
758 µs, max 19 ms; O_DIRECT 1 MiB sequential append 1768 MiB/s; append with
fdatasync per 64 KiB 1283 MiB/s.

Fan-out barrier, 64 KiB as 4x16 KiB to K owners, parallel, idle p50/p99: K=1
0.302/0.585 ms, K=2 0.488/1.353 ms, K=3 1.623/3.129 ms (max 26.2 ms). The
slowest owner was the ZFS host in 10 of 10 top-1% samples, median share of
barrier time 95.7% (BF:110-138). Derived planning floor for a 64 KiB fsync
through three owners: 5.9 ms idle, 3.8 ms busy; proposed SLO 10 ms p99.

### 2.4 Replication drain throughput

| Item | Value | Source |
|---|---|---|
| Legacy drain, earlier shape | 55.3 MiB/s | `CAS-DRAIN-THROUGHPUT-HANDOFF.md:29-37` |
| Legacy drain, current shape | 101.9 avg, 110.6 saturated MiB/s; batch lifetime 1.17 s; sender 94.5% idle | same |
| `put_keyed` share of batch lifetime | 899 ms, 76.5% | same |
| pgbench during legacy drain | lost ~90% throughput | `HANDOFF.md:55`, `CONTRACT.md:18` |
| Streaming drain, best 15 s window | 361 MiB/s = 3.03 Gbit/s at 151 KiB chunks | `PERFORMANCE-DESIGN.md:32-36` |
| Streaming drain, production small chunks | 108.7 MiB/s at 4,686 intents/s and 24 KiB; 120 MiB/s at 34 KiB | same |
| Target | 10 Gbit/s = 1,192 MiB/s, needs 36,800-51,400 intents/s at production sizes; demonstrated 9-13% of that rate | same |
| Sender accepted-frame round trip | 94-191 ms per frame, of which 60-170 ms had no phase owner | `PERFORMANCE-DESIGN.md:94-109` |
| Sender credit wait | 86-365 ms per wait | same |
| Class-blind pressure feedback (2026-07-31) | ceilings zero in 60-85% of samples, drain 12-20 MiB/s vs 101.9-110.6 baseline | `pressure.rs:25-28` |
| Single QUIC 5-tuple / eight endpoints | ~9-10 Gbit/s / 27.6-29.6 Gbit/s | `snapshot-design/11:164-166` |
| QUIC transport ceiling per bond direction | 25-27 Gbit/s | `CONTRACT.md:35` |
| vRack bandwidth-delay product | ~300 KB; 64 MiB window already fastest at 15.3 Gbit/s | `transfer.rs:51-62` |
| Dev drain, 5 peers, 2026-07-28 | ~830 intents/s = 51.4 MiB/s = 0.86% of a 50 Gb/s bond; flat ~281 ms per batch regardless of size | `snapshot-design/11:16-121,392-396` |
| Little's-law outstanding bytes for 10 Gbit/s at 1.17 s lifetime | 1.36 GiB | `HANDOFF.md:196-202` |

### 2.5 Drain and transfer constants (code)

Drain tick 1 s; page 512 intents; 4 pages in flight; 32 concurrent pushes; 10 s
peer timeout; steady:bulk 3:1 (unmeasured); frame 8 MiB; max batch put 24 MiB;
interested cap 32 MiB; total credit window 128 MiB; checkpoint 100 ms or 256
MiB; nondurable backlog cap 1 GiB; feeder queue 16,384 intents; read budget 64
MiB; local read concurrency 16; legacy pacer 128 MiB/s ("no doc comment, no
measurement"); QUIC bidi streams 16,384 (quinn default 100); IW 10; keep-alive
15 s (deck slides 7-8; `replica_intents.rs`, `streaming.rs`, `transfer.rs`).

### 2.6 Snapshot capture and restore (hil, 30 days)

| Item | Value | Source |
|---|---|---|
| Capture total, n=61 | p50 6.85 s, p90 17.1 s, max 60.1 s | deck slide 12 |
| Capture manifest phase (read dirty pages, hash, put) | p50 5.83 s, 85% of total | same |
| Guest pause | p50 8.8 ms, max 2,047 ms | same |
| Dirty pages p50 | 190,783 = 745 MiB, so 128 MiB/s = 4 threads x 32 MiB/s | same |
| Restore total, n=49 | p50 6.7 s, p90 106.5 s, max 236.5 s | deck slide 13 |
| Restores that hit warm memfd and do no CAS work | 13,720 of 14,055 | same |
| Worst CAS-backed restore drain | 12.4 GiB in 29.07 s = 437 MiB/s at ~9.8x parallelism | same |
| Fault stalls in that restore | 1,185 over 10 ms, 120 over 50 ms | same |
| Worst restore tail | 235 s of two dead timeouts, CAS drain finished in 2.7 s | same |
| Outbound intent backlog | 686,290 on Jul 30 to ~130 on Aug 2-3 | deck slide 15 |
| Pressure collapses in 14 days, three nodes | 10,128 / 8,329 / 5,542; 98.2% of collapse-minutes had no capture | same |

### 2.7 GC scale

~2M mark-absent candidates per hash shard, ~500M dead chunks region-wide;
`batch_size` 5,000,000; mark build ~20 minutes O(live set); ~105 bytes per
resident candidate; snapshot integrity must be fresher than `2 *
scrub_interval`; 3 warmup ticks; demotion measured safe at six-hourly, and the
2026-08-07 hil I/O incident was the combined hourly cost of node-sweep mark
plus demotion walks (`cas-gc-contract.md:1-13,440-449,473-475`).

### 2.8 Block volume (dev-compute-6, synthetic)

Overwrite storm, 32 MiB rewritten 16x in 64 KiB writes: fold every 1/4/16
passes gave 247/61.8/15.4 MiB into CAS; CAS-leg write amplification 0.482 flat
(synthetic repetitive content; real bytes "closer to 1x"). Map-node overhead
0.50/0.12/0.03% of coalesced bytes at those cadences. Index freeze with vCPUs
paused: 261 µs / 4.28 ms / 79.5 ms at 10 MiB / 100 MiB / 1 GiB dirty,
superlinear; dirty-entry cap set to 32,768 for an 11 ms freeze. Head publish
tick 5 s; a 4,096-hash replication confirm at ~2.4 s per 64-wide window needs
~155 s against a flat 30 s budget (blockvol WS-ENGINE-REPORT, MAP-SWAP-STATUS,
design/07, per the blockvol read).

The pre-log pathology that forced the intent log: guest fsync p99.95 9.6 s,
max 13.4 s; fold write amplification ~6x; log ingest 56-130x faster than CAS
drain; 6-20 s of acked writes at risk on crash; 225 MiB parked staging after
the workload ended with nothing to flush it (`W9-intent-log.md:19-35,146-147`).

## 3. Problems they hit, and where each stands

### 3.1 Write path

1. Class declared once, honored once. Three FIFO queues between the client
   socket and the disk pool ignore the foreground flag. Filed as an issue stack
   (#9674 parent). L0 instrumentation shipped (#9696). Status of L2 (gate
   `Mutex -> RwLock`) and L3 not recorded as landed in the docs I read.
2. The 3-40 ms gate wait with zero drain turned out not to be a priority
   problem. The gate is held across append plus commit (~1.6 ms) and puts to
   the same shard queue behind each other on a lock they do not need against
   each other. The recommendation changed from "add a comparator" to "make it
   an `RwLock`", with an explicit admission that the safety argument (locations
   name immutable sealed segments) "needs a test, not confidence"
   (`issue-stack.md:31-79,338-373`).
3. Group-commit linger deleted (#9700): 97% of closes unproductive against a
   16.5 µs raw commit. Put-coalescer linger kept because it saves 98% of
   commits. Same window, opposite verdicts, decided by measurement.
4. `with_class` does not cross `tokio::spawn`; unscoped defaults to
   Background. How much write-path work silently demotes is unknown (L5, open).
5. The pressure collapse is a compensator for two queues that lack a
   comparator. Plan: fix the queues, watch the indicators go flat for a week,
   delete the actuator, keep the indicators as metrics (L6, sequenced last).
6. Snapshot capture is a 190k-put foreground burst that, under strict
   two-tier priority, competes head-on with interactive guest writes. Whether a
   third tier is needed is deferred until measured (L4, unfiled).

### 3.2 Replication drain

7. Legacy drain held one page slot across the whole probe-to-fence chain per
   peer; sender 94.5% idle; pgbench lost 90%. Resolved by the streaming
   redesign (sessions, credits, tickets, horizons).
8. Class-blind pressure signal read the drain's own ingest as customer
   pressure and collapsed its own credit, 60-85% of samples at zero. Fixed by
   recording the two write-side waits per `DiskClass`
   (`pressure.rs:18-30`).
9. Byte pacer held its mutex across its own sleep, serializing all peers; a
   per-page barrier left six lanes idle. Fixed; pacer since retired with the
   streaming path (`snapshot-design/11:151-196`).
10. Kick/cancel/rewind drain urgency livelocked a node for 12 hours (ix#8330),
    was budgeted to 8 per pass, then deleted: "a mechanism whose safe
    configuration is mostly off is the wrong mechanism" (`snapshot-design/11:143-149`).
11. After streaming, 60-170 ms per frame has no phase owner, and the
    production shape is an intent-rate problem (24-34 KiB chunks), not a byte
    problem. Two sender copies and one receiver copy per payload are
    confirmed; the plan is instrument first, remove copies, remeasure, and
    explicitly refuses to change window, frame size, lanes, or credit policy
    without an isolated A/B (`PERFORMANCE-DESIGN.md:7-24,425-442`). Open.
12. Two unsafe watermark undercounts observed; treated as a correctness
    blocker independent of throughput (`PERFORMANCE-DESIGN.md:58-60`). Open at
    the time of writing.
13. Nix said an unset window preserved a 512 MiB default while code defaulted
    to 128 MiB; docs corrected, live default kept (#9327).

### 3.3 Durability

14. `FsyncPolicy::LocalAck` acked from RAM; the CAS store opened relaxed; the
    wire carried no LSN. "fsync lied by construction". Replaced by the stable
    tier and the intent log (`W6-durability-barrier.md:6-20`).
15. `AwaitDurable` meant "replica intents cleared into page cache". Renamed
    `present_on_owners` so the name stopped lying (`W6.1:12-15`).
16. The W6 end state said "no node-local WAL"; W9 reopened it after the 9.6 s
    fsync tail and an ENOSPC incident (`W9:1-9,22`).
17. Governor watched the intent log, not the replication queue, so a 25k
    backlog produced zero signal (`snapshot-design/11:114-120`).

### 3.4 GC

18. Refcounts removed in favor of a snapshot mark. The planning docs the code
    cites do not exist in the tree.
19. Reclaim disabled fleet-wide 2026-07-26. Per-candidate CAS fetch in the
    sweep made it O(dead bytes); removed. One-batch-per-tick made a 500M
    backlog a four-day drain paying a 20-minute mark per batch; drain loop
    added, default still 1 round per tick, "deploys dark".
20. Old-leader version skew could build a mark covering no chunk of a new
    source's class. Closed by storing the required source set in Postgres.
21. Demotion sweep plus node-sweep mark, both hourly, caused the 2026-08-07
    hil I/O incident. Divider added so demotion rides every Nth scrub.
22. Class-flip surplus was structurally invisible to segment-granular
    eviction (613,545 flips, zero bytes evicted). Chunk-granular tier 0 added.
23. A sentinel "empty TPM state" hash appeared as a dangling root in 145 VM
    rows; filtered.

### 3.5 Block volumes (the analog)

24. "FUA honored" in the spec was false; virtio-blk has no FUA.
25. Base image file as read base was accepted, then withdrawn: correct local
    reads, silently corrupt snapshots restored anywhere else. Replaced by
    build-time chunking of allocated extents and idempotent local seed.
26. "Chunk loss costs a redo" was wrong for a running VM: reads never consult
    the log, so a reclaimed chunk is a read failure on an acked write. Led to
    the fold-pin registry and two-phase drain.
27. A Merkle root cannot cover its own upload under a fail-closed mark; pins
    must name hashes terminally before the put (design/09:68-95).
28. Head registry keyed wrong so every boot reopened at base with the log
    truncated; volume dir deleted on stop so same-node restarts dropped the
    tail; recovery unlinked segments before re-append. All fixed, all found
    the hard way.
29. Read racing a fold returned IOERR, which is an XFS shutdown; changed to
    retry then wait on the fold permit.
30. A 2 TiB discard allocated ~536M spans, a guest-triggerable host OOM; 1 PiB
    fstrim was ~8.4M forced folds. Range tombstones and clamps.

### 3.6 Lazy store experiments (snix)

Not CAS proper, but the two snix docs record things the chunk store should
know. One file per blob at 4 KiB filesystem blocks cost 4.39x on disk for a
small-blob corpus (`docs/snix-lazy-store-pilot.md:250-262`), which is the same
lesson as V2 to V3 segments. Per-file laziness is only available if you trust
a daemon instead of verifying a whole-object signature, and over gRPC it was 7x
slower than fetching whole NARs because the bottleneck was per-blob round trips
at ~41 ms each, not bytes (`pilot.md:153-208`). `nix store gc` on the overlay
returned exit zero and collected nothing because `readdir` failed silently
(`snix-lazy-store-overlay.md:390-438`). A cache outage blocked 30 s per uncached
path (`pilot.md:212-218`).

## 4. Lessons for the research system

Format: ix learned X (ref); for the research system this implies Y.

### Staging log and the write ack

1. ix learned the fdatasync floor on PLP NVMe with ext4 is 35-90 µs p50 and
   under 0.2 ms p99 at 64 KiB, while ZFS with a mirrored SLOG is 4-10x slower
   at p50 with 15-64 ms maxima (BF:56-85, W9:27-29). The local-ack write path
   is only as good as the log device. Put the staging log on ext4 or raw NVMe,
   measure its fdatasync distribution at your batch sizes on the exact hosts,
   and report that floor as the first number in the paper.

2. ix learned the tail of any multi-owner synchronous ack is one owner's tail
   (95.7% median share, 10 of 10 samples), and that even two ext4 hosts had a
   single dominant owner (BF:129-138). Your no-network-on-the-write-path
   decision is the one ix converged on after measuring this. Keep it, and
   quote the K=2 parallel barrier (0.49 ms p50, 1.35 ms p99, idle) as the cost
   you avoid.

3. ix learned that with fdatasync at 35 µs, an artificial group-commit delay
   is slower than the sync itself, and that a 500 µs linger measured 97%
   unproductive (W9:98-101; `issue-stack.md:379-389`). Flush the staging log
   immediately when a waiter exists; use a periodic flush (ix uses 50 ms) only
   to upgrade unwaited writes. Do not add a linger to the ack path without
   measuring its fill.

4. ix learned that ack-from-page-cache plus one `fetch_max` LSN on the
   completion path plus one fdatasync on FLUSH is the whole durability story
   for virtio-blk, and that a write-through fallback is mandatory when the
   guest negotiates neither F_FLUSH nor WCE (blockvol design/01). Stock QEMU
   presents the same driver contract. Implement flush as "fdatasync through
   the max completed LSN", never per-write fsync, and never ack the whole
   appended tail.

5. ix learned a physical-redo-only log with per-record CRC where a torn tail
   stops replay gives prefix-consistent recovery mechanically, closing a proof
   obligation the previous design carried as an open property (W9:76-79,
   108-116). Keep the staging log identity-addressed and physical. Content
   addressing begins at the compactor, not in the log.

6. ix learned recovery must re-append before unlinking and needs a
   generation-complete marker, because power loss between unlink and
   re-append lost every acked write above the cut (blockvol recovery.rs:31-60
   per the blockvol read). Your compactor rewrites the log; give it the same
   completeness witness before it truncates.

### Compactor and chunking

7. ix learned coalescing latest-per-block before chunking is the whole
   economics: 512 MiB of guest writes became 15.4 MiB in CAS at a 16-pass
   cadence, and map overhead fell from 0.50% to 0.03% as folds got later
   (WS-ENGINE-REPORT, design/04). Fold late and bound the delay by
   time-to-durable, not by bytes. This is also where your dedup ratio will
   come from, so report it separately from cross-VM dedup.

8. ix learned CDC boundaries must snap to the block so no extent straddles a
   chunk, and that the loss is at most 4,095 bytes per boundary because guest
   repeats are block-aligned anyway (`blockvol/src/fold/chunking.rs:1-21`).
   For a block backend, fixed 4 KiB-aligned CDC is the right default, and the
   16/64/256 KiB sweet spot was chosen for source trees. ix never measured the
   chunk-size sweep for block workloads. That sweep is a cheap and publishable
   result.

9. ix learned production chunks are 24-34 KiB on the wire and the drain is an
   intent-rate problem, not a byte problem: 4,686 intents/s at 24 KiB was
   only 109 MiB/s, and 10 Gbit/s needs 37-51k intents/s
   (`PERFORMANCE-DESIGN.md:311-323`). Every per-chunk cost in your compactor
   and remote-fetch path (allocation, hash-set insert, per-chunk RPC, per-chunk
   catalog row) scales with that rate. Benchmark at production chunk sizes,
   not multi-megabyte ones, and batch per-chunk work into frames.

10. ix learned the receiver hashes every chunk with BLAKE3 and then the
    segment append scans it again for CRC (`PERFORMANCE-DESIGN.md:339-341`),
    and that two sender copies plus one receiver copy per payload were
    unconditional (`PERFORMANCE-DESIGN.md:214-309`). Design the frame encoding
    so chunk bodies are vectored slices of one owned buffer end to end, and
    make the CRC cover only the record prefix since the content hash already
    covers the data (`segment/mod.rs:33-38`).

### Chunk store layout and index

11. ix learned one file per chunk produced ~200M inodes and made cold ext4
    inode lookups the bottleneck; 32 GiB append-only segments with an inline
    `(len, hash, data, crc)` record fixed it (`segment/mod.rs:1-38`). The snix
    pilot hit the same wall from the other side, 4.39x on-disk amplification
    for small blobs. Do not store chunks as files. Pack them, and store the
    hash inline so recovery and compaction rebuild the index without
    re-hashing.

12. ix learned the index is the serialization point. LMDB single-writer
    envs, sharded 16 ways, still produced 3-40 ms queueing under pure put load
    because the gate was held across the commit (`issue-stack.md:31-79`). They
    also built an in-RAM hash table shadow to measure against the B-tree
    (`catalog/keydir.rs`). For a research system with an in-RAM
    offset-to-hash map and a hash-to-location index, keep the index a plain
    hash table in RAM, checkpoint it, and rebuild from segment records on
    crash. Do not put an on-disk B-tree on the compactor's path.

13. ix learned that opening the index `NOSYNC | WRITEMAP` lets the kernel
    persist index pages before the data they name, so recovery must
    distinguish fenced from unfenced entries and re-hash the latter
    (`W6.1:73-87`). If your chunk index is written lazily, order data-before-
    index at every fence, and on unclean start verify any entry not known
    covered. Content addressing makes "absent" recoverable and "served
    corrupt" not.

14. ix learned space reclaim in an append-only layout is copy-forward then
    unlink, never in-place, and that a segment packed in arrival order mixes
    hot, cold, owned and surplus chunks so segment-granular eviction found
    nothing to free (`compaction.rs:1-60`, `eviction.rs:24-41`). With epoch GC
    you will have the same shape. Consider packing by epoch so a whole segment
    dies with its epoch, and account dead bytes per segment from day one.

### Placement, remote reads, cache

15. ix learned deterministic placement (PG then straw2) keeps the coordinator
    off the data path and leader memory O(nodes), at the cost that `pg_num`
    can never change and a node without a map cannot decide where anything
    lives, which stalled puts on first boot (`placement/src/lib.rs`, archived
    audit case 1). Rendezvous hashing across two hosts is the same family. Make
    sure a host with a stale or missing map can still write locally and
    reconcile later, rather than parking.

16. ix learned dedup-skip requires unanimous presence on every current owner,
    that a non-owner cache copy never justifies a skip, and that an unreachable
    owner must answer "unconfirmed, upload" not "absent"
    (`leader_aware.rs:49-80`). Your per-host RAM cache is exactly the non-owner
    copy this rule excludes. Keep cache hits out of the dedup and GC decisions;
    they are read accelerators only.

17. ix learned the dedup probe and the delete barrier must see the same
    owner set, so map swaps are two-phase and re-affirmed before every delete
    batch (`cas-gc-contract.md`, placement-map barrier). If placement or
    epoch changes while GC runs, a producer on the old map can skip an upload
    against ex-owners while GC deletes from the true owners. Fence the map
    before any delete.

18. ix learned one QUIC 5-tuple carried ~9-10 Gbit/s on a 50 Gb/s LACP bond
    and that six endpoints were a link-aggregation trick, not QoS (deck slide
    8). If your two hosts use bonded links, one TCP connection will not fill
    them. If they do not, a single connection is fine and the lesson is
    inverted: measure the link before adding connections.

19. ix learned cold remote reads must never queue behind bulk transfer at any
    stage, and the two mechanisms that made the read path work were a
    dedicated connection for foreground reads and strict priority with a
    reserved floor at the disk (deck slides 9 and 18). A guest-blocking
    remote read in your design should have its own connection and its own
    admission class on the serving host, or the compactor's background pushes
    will convoy it.

20. ix learned the lazy-restore fault path did 437 MiB/s at ~9.8x parallelism
    with 5.06 ms per 231 KiB object and real guest stalls (1,185 faults over
    10 ms) at untuned concurrency constants (deck slide 13). Your cold-read
    latency will be dominated by per-object RTT plus serving-host queue time.
    Prefetch in extent order and measure the fault histogram, not just the
    throughput.

### Migration

21. ix learned migration by moving the head is the easy half; the shipped log
    tail and the fence ordering are the migration. The source must
    final-publish before the target acquires the writer fence, and a
    cross-node open must refuse when local records exceed the durable head
    rather than silently discard or silently serve (blockvol design/05,
    design/07). Moving the offset-to-hash map alone leaves every write since
    the last compaction on the source. Ship the staging-log tail, and define
    the fence.

22. ix learned a head registry must never be the sole source of state: a
    mis-keyed head row made every boot reopen at base and truncate the log,
    and a refusal keyed on writer identity kept healthy VMs from restarting
    on any node they had used (design/07:30-33,596-619). Resolve boot by
    evidence (local high-water LSN against the durable head), not identity.

23. ix learned the alive snapshot pause should contain only a queue quiesce,
    an atomic read and an index freeze, with the drain after resume, and that
    the freeze cost is per dirty entry and superlinear (261 µs at 10 MiB dirty,
    79.5 ms at 1 GiB) (design/05, WS-ENGINE-REPORT). If migration or
    snapshot freezes the dirty map, cap the dirty-entry count to bound the
    pause, and disk cut before RAM capture always.

### GC

24. ix learned refcounts were removed and the only safe discriminator is
    membership in a mark from one consistent snapshot, with every uncertainty
    resolving to a leak (`cas-gc-contract.md` sections 1, 5). Epoch GC is a
    reasonable substitute only if the epoch is a fence over both producers and
    the map, not a wall clock. Define what makes a chunk "in epoch E" and how
    a producer mid-upload at the epoch boundary is protected.

25. ix learned a Merkle root cannot cover its own upload under a fail-closed
    mark, so producers must name every hash terminally before the put
    (design/09:68-95), and that "chunk loss costs a redo" is false for a
    running VM because reads never consult the log once the map entry is
    retired (design/09:49-66). For epoch GC this means nothing put during
    epoch E may be collectable before the map commit that references it is
    itself durable, and the compactor must pin its in-flight chunk set, or
    hold the staging-log entries until the map commit lands.

26. ix learned the resurrection race (producer skips an upload because an
    owner still reports present, GC deletes moments later) needs a two-phase
    quiesce: mark delete-pending on every owner so dedup answers absent, then
    re-validate, then delete only on barrier-id match (`cas-gc-contract.md`
    section 4). Even with epochs, the dedup probe must see delete-pending.

27. ix learned the transfer receiver must pin ticketed hashes until a durable
    horizon covers them, because a reclaim between acceptance and checkpoint
    could publish a horizon over a deleted chunk and the sender would retire
    the last copy (C-R-PIN). If your compactor pushes to a remote host and
    retires local staging on ack, the remote's GC must not be able to delete
    what it just acknowledged before it is fenced.

28. ix learned a full mark is O(live set) at ~20 minutes and that a
    per-candidate existence fetch made the sweep O(dead bytes); one batch per
    hourly tick meant a 500M-chunk backlog would take four days (section 6).
    Design the GC so its cost is proportional to what it deletes, and so a
    tick can drain many batches. Also budget the mark's I/O: two hourly walks
    together caused an I/O incident.

29. ix learned GC-disabled is the shipped default and "observe" mode (build
    and serve the mark, delete nothing, report what would go) is how the
    flip is earned (section 2). Build observe mode into the research system;
    it is also how you get the dedup and dead-space numbers for the paper.

### Backpressure and classes

30. ix learned the write log absorbs 56-130x faster than the chunk store
    drains, so a debt governor with `W = drain_rate x T_catchup`, a floor, and
    a hard park ceiling is required, and that a constant drain-rate estimate
    (220 MiB/s assumed, 128 MiB/s process-wide cap) made the governor unable to
    converge (W9:125-158; `replica_intents.rs:69-111`). Measure the drain rate
    the compactor actually achieves and pace on that. Never pace on a
    constant.

31. ix learned parked debt is a crash window that sits still when the system
    looks safest: 225 MiB stayed in staging after the workload ended because
    nothing triggered a flush (W9:146-149). Give the compactor an idle
    trigger so time-to-durable after quiescence is bounded.

32. ix learned a class-blind pressure signal reads the system's own
    background ingest as foreground harm and collapses itself, and that a
    control loop compensating for FIFO queues is strictly worse than priority
    queues (pressure.rs:18-30; deck slide 18). If you add a pressure signal,
    tag every wait with the class of the work that waited, and prefer
    structural priority at each queue over a global collapse.

33. ix learned that urgency expressed by cancelling in-flight work livelocked
    a node for 12 hours, and that a pacer mutex held across its own sleep
    serialized every peer (`snapshot-design/11:143-196`). Interest raises
    priority for the next dispatch; it never cancels. No lock across a wait.

### Measurement discipline

34. ix learned to publish distributions with N and load state, keep outliers
    in the tables, commit raw samples with hashes, and label "planning sum of
    quantiles" as not a p99 (BF:157-172). Their own docs repeatedly caught
    themselves: counters that reported 0.00% because the measured plane
    moved, a fault-injection harness that tested recovery while the fault was
    still installed, a blackhole route that did nothing because a policy
    table came first (design/04:150-158; overlay.md:380-388;
    pilot.md:220-227). Assert a nonzero floor on every counter and verify
    every injected fault took effect before trusting the result.

35. ix learned that matching arithmetic (128 MiB/s = 4 threads x 32 MiB/s)
    is consistent with a hypothesis but does not prove it, and the causal
    test is to change the constant and watch the number move (deck slide 12).
    Their throughput plan refuses to ship any transport change without an
    isolated A/B on the deployed generation (`PERFORMANCE-DESIGN.md:16-19`).
    For the paper, every claimed bottleneck should come with the sweep that
    moved it.

36. ix learned that the largest measured restore cost (235 s) was two dead
    timeouts above the storage layer while the CAS drain took 2.7 s, and that
    two popular hypotheses ("pressure resets slow snapshots", "memory-heavy
    snapshots stall on paging") were refuted by the data (deck slides 13,
    17). Instrument the whole path, including the parts you did not build,
    before attributing latency to the storage design.

## 5. File map

| Topic | Path |
|---|---|
| Stack model, queue inventory, measurements | `docs/architecture/cas-storage-model.html` (slides in the JS `slide()` calls) |
| Write-path plan and #9700 linger deletion | `docs/architecture/cas-write-path-issue-stack.md` |
| GC contract | `docs/architecture/cas-gc-contract.md` |
| Drain: legacy measurements | `docs/cas-drain/CAS-DRAIN-THROUGHPUT-HANDOFF.md` |
| Drain: streaming contract (C-* items) | `docs/cas-drain/CAS-DRAIN-STREAMING-CONTRACT.md` |
| Drain: post-streaming bottlenecks and ship plan | `docs/cas-drain/CAS-DRAIN-PERFORMANCE-DESIGN.md` |
| fsync floor | `docs/research/vcfs-respine/BARRIER-FLOOR.md`, `W6.0-BARRIER-FLOOR.md` |
| Stable tier design | `docs/research/vcfs-respine/W6.1-STABLE-TIER.md`, `W6-durability-barrier.md` |
| Intent log and governor | `docs/research/vcfs-respine/W9-intent-log.md` |
| Drain classes, dev drain run | `docs/snapshot-design/11-replication-drain-classes.md`, `00-measurements-brownout-run.md` |
| Chunking | `crates/storage/cdc/src/lib.rs`, `crates/storage/cas/src/chunker.rs` |
| Segment format | `crates/storage/cas/disk/src/segment/mod.rs` |
| Catalog, group commit, keydir | `crates/storage/cas/disk/src/catalog.rs`, `catalog/group_commit.rs`, `catalog/keydir.rs` |
| Disk pool classes | `crates/storage/cas/disk/src/diskpool.rs` |
| Stable fence rounds | `crates/storage/cas/disk/src/stable.rs` |
| Put coalescer | `crates/storage/cas/disk/src/put_coalesce.rs` |
| Eviction, compaction | `crates/storage/cas/disk/src/eviction.rs`, `compaction.rs` |
| Placement | `crates/storage/cas/placement/src/lib.rs` |
| Placement-aware store, dedup unanimity | `crates/storage/cas/fabric/server/src/leader_aware.rs` |
| Drain constants | `crates/storage/cas/fabric/server/src/replica_intents.rs`, `replica_intents/streaming.rs` |
| Pressure loop | `crates/storage/cas/fabric/server/src/pressure.rs` |
| Transfer receiver | `crates/storage/cas/fabric/server/src/transfer.rs` |
| Leader | `crates/storage/cas/fabric/leader/src/lib.rs` |
| Block volumes | `crates/storage/blockvol/README.md`, `blockvol/design/01..09`, `MAP-SWAP-STATUS.md`, `docs/block-volumes/` |
| Lazy store experiments | `docs/snix-lazy-store-pilot.md`, `docs/snix-lazy-store-overlay.md` |
| Deferred credits | `docs/architecture/deferred-credits.md` (billing admission; not storage) |
| Archived PG-index design fight | `docs/_archive/design/agent-judgment-audit.html`, case 1 |

`docs/architecture/deferred-credits.md` is about billing credit pools and VM
create admission, not storage credits. Its one transferable idea is the
staleness guard: a push-fed projection that degrades to the synchronous path
after `T_stale` rather than failing open. `docs/architecture/guest-kernel/` and
the rest of `docs/_archive/design/` contain no CAS material beyond the audit
case above.
