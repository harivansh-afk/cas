# segstore lessons 1 through 14: what they say and what they imply for a two-host CAS block backend

Source: https://internal.indexable.workers.dev/segstore/ (lessons 01-block-lie through 14-guest-view), fetched 2026-09-01. Body text is quoted from the server-rendered HTML. Quiz answer keys and the hidden "say it from memory" answers come from the site's JS bundle (`_app/immutable/chunks/OP7jwocT.js`), where each option carries a `why` explanation and the correct one is flagged `ok`. Where I quote an option's explanation I label it "(quiz explanation)".

The course is a 28-lesson design walkthrough of a fleet-scale system (100k VMs, ~500 compute nodes, a separate storage tier). Nothing in it is a two-host system. Each "for the two-host research system" line below is my mapping; the "segstore does X because Y" half is theirs.

Terms used throughout, as segstore defines them:

- **segment**: a 256 MiB append-only region; open while growing, sealed when full, then immutable and named by the hash of its bytes.
- **root record**: per VM, "a pointer to its parent snapshot plus the zones this VM wrote itself". Kilobytes.
- **index**: "segment name → where those bytes physically are, plus its reference count".
- **journal**: replicated small-append log on storage-server SSD; the thing a fleet-class fsync waits for.
- **class**: local, fleet, or archive; "how far an ack travels".
- **cooling**: sealed segments waiting on compute NVMe to see whether their VM dies before anyone pays to destage them.
- **destage**: erasure-code a sealed segment to HDD.
- **E / D**: the durable watermark and the destaged watermark, two per-VM integers.

---

## Lesson 1. The block-device lie

### Summary of the argument

The lesson's premise is that in-place overwrite is a fiction every layer maintains at cost: "Every storage layer already fakes in-place writes. The SSD firmware, dm-thin, every copy-on-write filesystem: each keeps a hidden location map, a hidden garbage collector, and hidden latency spikes. Stack a VM image on top and the same work happens two or three times."

The design response is one sentence the rest of the course derives from: "Nothing overwrites. The guest appends, the disks stream, and there is exactly one garbage collector, in the open." And: "Every mechanism in this course is a consequence of taking that sentence seriously."

The block API promises three things no medium can deliver: "a flat array of addresses, every one writable in place", "uniform cost per address", "an address that exists before anyone wrote it". What media can do: "flash: program a page once, erase in megabyte blocks", "shingled HDD: overwriting a track destroys its neighbour", "any HDD: ~8 ms to move the head, then it streams". "The gap between the columns is where every hidden translation layer lives. Delete the promise and the layers have nothing to hide."

The "go deeper" box names the block API's third sin, the provisioning leak: "a chunk that is addressable but only partly written exposes a previous tenant's bytes unless somebody zeroes it, which is why dm-thin has a zeroing tax. In an append-only API 'addressable but unwritten' cannot be expressed, so the leak and the zeroing pass both vanish by construction."

The interactive simulator's rules, stated in prose: "the old version is never touched, it just becomes garbage where it lies. A segment seals when it fills (or when you seal it). GC collects only sealed segments: it copies their live cells forward, same version, ringed, and frees the segment whole. Those copies are write amplification."

### Numbers, invariants, laws

| item | value |
|---|---|
| read 4 KiB (transfer only) | 0.02 ms |
| one HDD seek | ~8 ms |
| stream 1 MB | ~5 ms |
| stream 32 MiB | ~160 ms |

Stated rule: "why everything here batches: one HDD seek buys a megabyte of streaming".

Invariants: nothing overwrites; exactly one garbage collector, visible; GC operates only on sealed segments and frees whole segments; "addressable but unwritten" is unrepresentable.

Quiz key: overwriting one byte on an SSD means "The new data lands somewhere fresh, a hidden map is updated, and the old page becomes garbage." Quiz explanation: "Every SSD runs a secret append-only store (the flash translation layer) to fake overwrite. segstore does that work once, in the open, and makes the guest part of it."

### Lesson for the design

segstore refuses to expose in-place overwrite at all because every layer that fakes it adds a hidden map and a hidden collector, and stacking them multiplies the work. For a two-host research system this implies: the local append-only staging log is the right shape, but only if it is the *only* append-only layer with a collector. If the chunk store on top also rewrites in place, or the RAM cache keeps its own map, the design has two hidden collectors again. The "one collector in the open" claim is a property to test for, not a slogan.

The provisioning-leak point applies directly: a thin block backend that presents unwritten LBAs must synthesise zeros rather than read whatever an allocated-but-unwritten chunk holds. Lesson 14 restates this for the plain-disk view.

The seek and stream numbers are HDD numbers. The two-host system is NVMe on both ends, so the "one seek buys a megabyte" law does not bind; batching still matters for network round trips, which is lesson 5's argument.

---

## Lesson 2. Requirements

### Summary of the argument

The headline numbers "pick the design before we do". The workload is "100k VMs, 5 min mean life, 1 vCPU and 8 GiB each · 333 spawns and 333 deaths per second every VM forked from another". Ancestry is "one tree: every VM descends from a single first image · fork chains run ~10,000 deep", with the constraint "reads must not walk the chain; metadata must not sit in one shard". Compute nodes have "local NVMe, assume 2 TiB · staging, cooling and read cache share it", and are "RAM-bound: ~250 VMs per node". Storage servers are "HDDs plus SSD · adding one changes placement policy and nothing else". Guests run "our Linux kernel: mainline plus segstore patches · files ride zoned XFS · anything needing a patch is fair game".

Product promises: fork and snapshot are "instant, host-owned, zero guest cooperation · works on a hung guest"; mobility is "a pointer moves, not a disk"; capacity is "up to 128 TiB per VM, thin"; isolation is "no data leaks, no dedup oracles, quota isolation · content addressing throughout"; failure is "clean, attributable refusal · never corruption, never silence · a wrong answer beats no answer only in demos"; availability is "any one compute or storage box can be killed at any moment, unplanned · nothing user-visible stops except the VMs that were on a dead compute box, and those come back without a human".

The "Kill any box" table is the availability test. A compute box: "its ~250 VMs stop; every other VM in the fleet notices nothing the control plane restarts them elsewhere from their roots; undestaged fleet-class writes replay from the journal group in minutes; local class is gone by contract". A storage box: "nothing user-visible reads reconstruct around its shards, placement stops choosing it, its journal groups run at 2 of 3 and get a third member". A control-plane member: "nothing each consensus group has three members on three different boxes". Two boxes at once: "not promised a group that loses two of three stops accepting forks and creates for its family trees; running VMs keep running; nothing acked is lost". The rule drawn from it: "a design that makes any single box the reason something stops is wrong, however elegant. The box count it implies is in the last unit: three storage boxes for the control plane to have a majority no box can take, four for a box to be dark on purpose."

"Two terabytes, three tenants" is the flash budget: "The flash on a compute node is the tightest constraint in the system. Three things want it: segments still being written, sealed segments waiting to see whether their VM dies, and read cache." The cooling window is defined as a space budget, not a timer (quiz explanation: "The window is derived from free flash, so a burst shortens it on its own. The cost is destaging some data that would have died anyway, which is the correct trade when flash is scarce."). A tenant may not choose residency: "A tenant choosing how long their dead data occupies someone else's flash is a denial-of-service knob. Durability class is the tenant's choice; residency is the node's."

The two knobs a small disk forces: "Keep active zones per VM in single digits: a half-filled open segment pins flash it is not using, and 250 VMs with many zones open eat hundreds of gigabytes before any cooling backlog exists. And let destage start early under pressure rather than letting staging overflow into a stall. Early destage wastes some HDD writes on data that would have died; a stall costs every VM on the node."

The arithmetic section derives the fleet from four inputs (VM lifetime, unique bytes per life, survivor fraction, chain depth). The key consequence for durability: writes are "~512 MB unique per VM life, measured on the target workload · × 333 deaths/s ≈ 170 GB/s fleet-wide, ~340 MB/s per node", which is "comfortable on local flash" but "if all of it were journaled far beyond any NIC · fleet durability for everything is impossible so durability is a per-volume choice".

### Numbers, invariants, laws

| item | value |
|---|---|
| concurrent VMs | 100k |
| spawn = death rate | 333/s ("100k ÷ 5 min") |
| VM life mean | 5 min |
| per VM | 1 vCPU, 8 GiB |
| ancestry | one tree, chains ~10,000 deep |
| compute nodes | ~400–500, ~250 VMs each, 2 TiB NVMe |
| bytes ever reaching HDD | ~1% ("a lifetime-distribution fact, not a derivation") |
| unique bytes per VM life | ~512 MB (measured) |
| fleet write rate | ~170 GB/s; ~340 MB/s per node |
| destage rate | ~1.7 GB/s fleet-wide ("one storage server's spindles stream it") |
| skip node interval | every ~32 levels; caps reads and delete cascades at ~32 hops |
| flatten rate | 333/32 ≈ 10 flattens/s, each "up to ~10⁶ segments in one batched commit" |
| merged table | 524,288 zones × 16 B ≈ 8 MiB, + 32 B per distinct segment ≈ 24 MiB for a full disk |
| flash budget example | cooling backlog 102 GB, open segments 128 GB, read cache 1818 GB at 340 MB/s, 5 min window, 4 active zones per VM; "the disk would hold up to 94.1 minutes of cooling" |
| max VM disk | 128 TiB thin |
| minimum boxes | three storage boxes for consensus majority, four for one to be dark on purpose |

Invariants: fork and reclaim are O(1); "a fork never crosses a shard"; "children are created in the parent's shard"; "flattening is mandatory, not lazy"; forbidden reads are identical to not-found; cooling is a space budget.

Quiz keys: a fork spends its time "Writing one small metadata record" ("A fork copies a root record (kilobytes) and adds one reference to the parent."). A 10,000-deep read "resolves in a bounded number of hops, because every ~32 levels an ancestor carries a fully merged map of the disk". Losing one control-plane member costs "Nothing". The cooling window is "A space budget".

### Lesson for the design

segstore makes durability a per-volume choice because its measured write rate is "far beyond any NIC" if everything were journaled. For a two-host research system this implies: the "ack locally vs mirror before ack" question is not one decision. segstore's answer is *both*, selected per volume, with local-ack as the explicit lower class and mirror-before-ack as the default. A research system can offer the same two classes and measure them against each other rather than picking one. If it offers only local-ack, it should say so as a class ("local class is gone by contract"), not as an unstated exposure.

segstore treats staging residency as a space budget owned by the host, not a timer and not a tenant setting. For the two-host system this implies the compactor's trigger should be free space on the staging device with an early-start under pressure, and any "compact after N seconds" timer is a derived default, not the mechanism. The stated cost of early compaction ("wastes some HDD writes on data that would have died") maps to wasted chunking and hashing work.

The survivor fraction (~1%) is the one input that makes cooling pay. It is a property of a 5-minute-VM workload, not a law. For a research system with long-lived VMs the fraction is unmeasured and may be near 100%, in which case cooling before compaction buys nothing and the compactor should run continuously. This is worth measuring before adopting a cooling stage.

The "Kill any box" table does not apply at two hosts as written (it needs three boxes for consensus and four for placement), but the test format does: pick a host, pull its power, list what stopped, and every item must recover without a human. At k=2 across two hosts, one host dying leaves exactly one copy of everything and no quorum for anything; the report should state that plainly rather than promise availability.

---

## Lesson 3. Three objects, five laws

### Summary of the argument

"Everything in the system is one of three things." A **segment** is "a 256 MiB region that data is appended into · open while growing, on compute NVMe; sealed when full, never changed again", "the unit of storage, and the unit of deletion". A **root record** is "per VM: a pointer to its parent snapshot plus the zones this VM wrote itself · kilobytes", and "every read starts here; a flattened full table is a separate immutable object". The **index** is "segment name → where those bytes physically are, plus its reference count · owned by the daemon that holds the drives". The compression of the whole thing: "Roots name; the index locates." A **name** is "(domain, hash of the plaintext) · the domain is the tenant or a declared public set"; "a bare hash means nothing on its own".

"Segment bytes are frozen at seal. A root changes by atomic swap; the index changes as segments move. Both changes are journaled."

The unit ladder: 4 KiB block ("the smallest thing that can be live or dead", "nothing here is a sector: there is no 512 B anything"), 32 MiB shard ("the unit of placement, repair and scrub", "never freed on its own"), 256 MiB segment ("the unit of naming, reference counting and deletion", "the collector works here, like an SSD works in erase blocks"), the pool ("quotas and the pressure ladder are measured here"). And the sentence that surprises people: "The collector is segment-based, never block-based. A block becomes garbage the moment it is overwritten, but nothing is freed until every block in its 256 MiB segment is garbage or somebody copies the survivors out."

The five laws, verbatim headings with their gloss:

1. **nothing overwrites**: "every write appends into a segment that only grows · freeing space means deleting a whole segment · no data byte is modified in place at any layer; metadata changes are journaled appends too".
2. **two kinds of names**: "things that still change are found by place: 'the root for vm-17' · a sealed segment is named by the hash of its bytes · any copy anywhere can be checked against its own name".
3. **a node can die**: "by the time we ack, the bytes are on a storage server too · unless the volume chose local class on purpose · why node death and live migration are one procedure".
4. **the expensive operations are table edits**: "snapshot copies a root; fork copies it and lets both keep writing · revert swaps it back; migration moves it · data moves later, in the background, or never".
5. **failure is refusal**: "a full pool rejects writes cleanly, over-quota tenants first · data you may not see returns 'not found', exactly like data that does not exist · nothing stalls silently or degrades in secret".

The sharp end of law 5: "A refusal you may not see is byte-identical to a thing that never existed. Any difference between the two answers is an oracle that tells one tenant what another one stores."

### Numbers, invariants, laws

Unit ladder: 4 KiB block, 32 MiB shard (8 data + 3 parity per segment), 256 MiB segment, the pool. Root record size: kilobytes.

Invariants: the five laws above; segment bytes immutable at seal; root changes by atomic swap; open segments are addressed by (node, id), sealed by hash; the collector never frees below a segment; the name includes a domain.

Quiz keys: overwriting one 4 KiB block frees "Nothing, until the whole 256 MiB segment is garbage or a collector copies the survivors out" (explanation: "One overwritten block changes a count, not the disk. This is the granularity that makes fork and reclaim O(1), and it is also why a long-lived database can pin space it no longer uses."). An open segment cannot be hash-named because "Its bytes are still changing, so the name would change under everyone holding it" (explanation: "Durability and naming are separate axes. An open segment on a fleet-class volume is fully durable via the journal, and it still cannot have a content name."). Forbidden equals not-found because "A distinguishable refusal is an existence oracle". Recall answer: "Law 3. By the time a fleet-class write is acknowledged, its bytes are already on a storage server. So a compute node can burn and nothing acknowledged is lost, which is also why node death and live migration are the same procedure."

### Lesson for the design

segstore names by content only after seal and by place before, because a content name is only stable if the content is frozen. For a two-host system whose compactor "chunks/hashes/dedups", this is the same split: the staging log is place-addressed (host, log offset), the chunk store is content-addressed, and the boundary between them is the seal. The lesson's added point is that durability and naming are independent: bytes can be fully durable while still unnamed. That decouples the "mirror before ack" question from the "when do we hash" question. Mirroring can act on raw log ranges; hashing happens later.

segstore deletes only at segment granularity because that is what keeps fork and reclaim O(1), and it accepts the slow leak as the price. This bears on "segment vs chunk granularity": segstore's unit of naming, refcounting and deletion is the 256 MiB segment, not a small chunk. A chunk-granular dedup store has finer reclaim and far more refcounts to keep consistent. Lesson 11 gives the cost model they used to pick 256 MiB.

Law 5 (refusal, not silent degradation) applies at any scale: when the peer host is down, a k=2 system should either refuse writes that require the peer or downgrade to a named class, and the guest-visible ack must say which. Law 3's "node death and live migration are one procedure" is only true if a node's acked bytes are somewhere else; under local-ack it is false, and the report should say so.

The domain-qualified name and the forbidden-equals-not-found rule are multi-tenant properties. A single-tenant research system can note them and skip them.

---

## Lesson 4. Topology

### Summary of the argument

Two tiers. Compute is "~300–500 nodes, ~250 VMs each", each with "VMs two virtio disks and /cas each", a "segstore daemon vhost-user backend", and "NVMe · 6 × PM1743, FDP open segments, cooling, local class, read cache", and the NVMe is "private: never pooled, never meshed". Compute nodes are "identical, disposable"; "nothing unique lives here for fleet-class volumes". Storage is "scale out by adding boxes; placement policy adjusts, formats do not", each with "journal · FDP NVMe fleet acks land here first", "metadata + warm cache · SSD roots, refcounts, placement; hot destaged reads", and "HDDs · 36 × Exos X24 EC shards, no RAID underneath". Between tiers: "plain TCP between the tiers. never NVMe-oF, iSCSI or NBD journal.append · shard.put / shard.get · pin / unpin · have() · root records · scrub".

The link missing on purpose is compute-to-compute. Quiz explanation: "Pooling NVMe across nodes rebuilds a storage fabric, couples failure domains, and tempts everyone to treat cache as durable. Nodes stay independent and disposable."

Each medium has one job. Compute NVMe: "private to its node · cache and staging only fleet-class data is never uniquely here · lose the node, lose nothing acked". Journal SSD: "small appends, latency-critical · the second failure domain what makes an ack an ack · terabytes per server, not small". Metadata SSD: "placement is rebuildable by listing the drives; roots are not, so they get the replicated log"; "segments do not know their owners". HDDs: "sealed segments as EC shards · streamed, never seeked for writes · the only tier that is cheap per byte".

The cross-tier protocol, verbatim:

```
/// compute daemon -> storage server, plain TCP (RDMA optional, journal first)
trait StorageServer {
fn journal_append(&mut self, vm: VmId, seq: u64, bytes: &[u8]) -> Acked;  // group-committed
fn journal_trim(&mut self, vm: VmId, upto: u64);                            // drop <= D
fn shard_put(&mut self, hash: Hash, k: u8, bytes: &[u8]) -> Placed;        // reserve, then stream
fn shard_get(&self, hash: Hash, k: u8, off: u64, len: u64) -> Bytes;
fn pin(&mut self, hash: Hash, owner: Root);      // per-owner set, not a counter
fn unpin(&mut self, hash: Hash, owner: Root);
fn have(&self, hashes: &[Hash]) -> Vec<bool>;    // batched, never one round trip per name
fn scrub(&self, hash: Hash) -> Verdict;
}
```

"Everything that crosses between the tiers is one of these calls, and every one of them is idempotent by name."

Where state lives: "Authoritative state lives only where loss is not routine. The compute tier is disposable by design, so nothing authoritative may live there." Control plane RAM holds "VM → node, VM → journal group, root records, membership · authoritative consensus groups on the storage tier, never on compute". Storage daemon RAM holds "hash → drive and slot, refcounts, blob index · authoritative for its own drives about half a gigabyte per petabyte · rebuildable by listing the drives". Compute daemon RAM holds "open segments, cached roots, connection pools · a cache, never a source dies with the node and costs nothing acked · RAM here is sold to guests, not spent on bookkeeping". "None of it is a database. Every authoritative table is RAM plus a replicated log plus periodic checkpoints, the same recipe the storage tier uses for its own metadata. One durability system, not two."

Where a node sends things: "A VM never connects to a storage server. The guest speaks virtio to the daemon on its own node; only the daemon crosses the network, and mostly it makes no choice at all, because the destination is recorded." Reading a shard: "the index already says which server and slot holds it · no choice to make · the placement record is the routing". Destaging: "placement policy picks servers once, at seal · anti-affinity, then free space and load · the one real decision, made per segment". Journal appends: "every VM has an assigned journal group · a fixed trio of servers · stable for the VM's life; changes only on failure".

Why the journal has a fixed home and shards do not: "Shards want freedom: spread by load, rebuild elsewhere when a drive dies. A journal wants the opposite, because replay has to find every entry in order. So a VM gets a journal group at creation and writes to it for life. Lose quorum and fleet-class writes refuse rather than quietly downgrading."

Break it yourself: "placement never puts more than three of a segment's eleven shards on one server (the parity budget; with eleven or more servers it is one each)". "Kill a second server and you are asking eleven minus six to cover eight, which it cannot."

Why the tiers are asymmetric: "The expensive property, durability, is concentrated where it can be engineered once; the disposable property, speed, is spread where failure is routine."

### Numbers, invariants, laws

| item | value |
|---|---|
| compute nodes | ~300–500, ~250 VMs each |
| compute NVMe | 6 × PM1743 per node |
| storage HDDs | 36 × Exos X24 per box |
| storage daemon index RAM | ~0.5 GB per PB |
| shards per segment | 11 (8+3); max 3 on one server; need 8 to read |
| journal group | fixed trio of servers per VM, for the VM's life |

Invariants: authoritative state only where loss is not routine; compute RAM is a cache, never a source; every cross-tier call is idempotent by name; `pin` is a per-owner set, not a counter; `have` is batched; no compute-to-compute link; the journal is fixed-home, shards are free; loss of journal quorum means refusal.

Quiz key: the missing link is "Compute node to compute node".

### Lesson for the design

segstore forbids compute-to-compute data links and keeps every durable byte on a separate storage tier, because pooling node-local flash "rebuilds a storage fabric, couples failure domains, and tempts everyone to treat cache as durable". The two-host research system is exactly the compute-to-compute mesh segstore leaves out: two peers, each holding the other's replicas, each a compute node. This is not a reason not to build it (the research question is whether it works), but it is the sharpest contrast in the course and the report should name it. The concrete hazard segstore identifies is the third one: a hash-keyed RAM cache on each host is, by segstore's rule, "a cache, never a source", and any write path that counts a peer's RAM as a replica has crossed that line.

segstore gives the journal a fixed home and lets shards roam because "replay has to find every entry in order". For a two-host system this splits the mirroring question in two. If writes are mirrored before ack, the mirror target must be fixed per VM and the mirror stream must be replayable in order; it cannot be a rendezvous-hashed chunk placement. Rendezvous hashing is the right tool for the *sealed chunk* placement (segstore's `shard_put`), not for the staging mirror.

The protocol shape transfers whole: every cross-host call idempotent by name, `have` batched, pins as per-owner sets rather than counters. The last one matters most for a dedup store; a counter cannot be made idempotent under retry, a set can.

"Placement is rebuildable by listing the drives; roots are not" is a useful test for what needs replication: the two-host system's chunk index can be rebuilt by scanning the chunk store, but the per-VM map (LBA to chunk) cannot, so only the latter needs the replicated log.

---

## Lesson 5. The journal at scale

### Summary of the argument

"Every fleet-class fsync lands on a journal before it acks, and no three servers can take that." The answer is sharding: "the journal is sharded into hundreds of groups; each VM writes to its one assigned group". Quiz explanation: "Each storage server hosts slices of several groups, a VM inherits a group its node already talks to, and group commit batches a node's VMs into one flush. Losing one group's quorum stalls only that group's VMs." The wrong answer, least-loaded server per write, is wrong because "replay has to find every entry for a VM in order, and the fsync path wants a warm connection".

Demand: "100k VMs × ~5 fsyncs/s ≈ 500k appends/s × 3 replicas = 1.5M server-side appends/s". Only the fsynced minority crosses the wire: "most churn is local-class and never crosses the wire". One server: "group commit: a window of appends becomes one SSD flush · about 100k appends/s sustained ~1–2k flushes/s × ~50–100 appends each", so "demand is roughly 15 servers of work". The fleet: "hundreds of groups over ~100 storage servers · each server carries ~15k appends/s about 15% of one SSD flush budget, 4 Gbps of its 50 · headroom, not heroics".

Bytes: "assume ~10% of written bytes are fleet class: 17 GB/s × 3 replicas = 51 GB/s · over ~100 servers ≈ 0.5 GB/s each, 4 Gbps of the 50 appends average tens of KB after group commit · the assumption is the input; measure it."

Retention: "an entry lives until its segment destages: the cooling window plus destage lag · 0.5 GB/s × ~49 min ≈ 1.5 TB per server, plus headroom · the journal SSD is sized for this, not for latency alone."

Trim: "entries are kept in per-VM extents inside the group · so a slow VM cannot pin the whole group behind its tail · trim is per VM, one pointer move each."

"Two details keep sharding cheap. Assignment is node-affine: a new VM takes a group its node already streams to, so a node holds about three journal connections rather than three hundred. And appends group-commit, so every VM on the node shares those connections and the server batches a window into one flush."

The wire: "Two 25 Gbps NICs per box, bonded. A bond is not a 50 Gbps pipe for one conversation: LACP hashes each flow to one link. Our traffic is already many flows, and the classes are kept on separate connections so a 32 MiB shard transfer never sits in front of an fsync." Four classes: journal ("small synchronous appends · own connections, latency-classed · this is what an fsync waits for"), destage ("32 MiB shards, eleven at a time · pooled bulk connections · single-digit MB/s per node in steady state"), repair ("bulk, rate-limited · the real reason to want 50 Gbps"), reads ("only cache misses reach the wire · bursty after a migration or a cold start").

"340 MB/s per node is a disk rate, not a network rate: every write lands on local NVMe, only the fleet-class minority is journaled, and only the ~1% that survives cooling is destaged. Steady state is nowhere near 50 Gbps; the NICs earn their keep in events."

RDMA: "RoCEv2 on this hardware turns a journal round trip from hundreds of microseconds into tens. The cost is operational: RoCE wants PFC or ECN tuning and fails in ways harder to debug than a TCP retransmit. So TCP is the always-works path and RDMA is an accelerator, journal first. Rejecting NVMe-oF was about semantics, not transport: it exports raw block addresses and hands placement, integrity and failure handling back to the client. Our protocol over RDMA keeps named segments and idempotent operations and only changes how bytes arrive."

### Numbers, invariants, laws

| item | value |
|---|---|
| fsyncs per VM | ~5/s |
| fleet appends | ~500k/s; ×3 replicas = 1.5M/s server-side |
| one server sustained | ~100k appends/s, ~1–2k flushes/s × ~50–100 appends per flush |
| servers of work | ~15; spread over ~100, ~15k appends/s each |
| fleet-class share of bytes | ~10% (assumed; "measure it") |
| journal bytes | 17 GB/s × 3 = 51 GB/s; ~0.5 GB/s per server, 4 Gbps of 50 |
| append size after group commit | tens of KB |
| retention | cooling + destage lag ≈ 49 min; ~1.5 TB SSD per server plus headroom |
| NICs | 2 × 25 Gbps bonded, LACP per-flow |
| RDMA journal RTT | "hundreds of microseconds" (TCP) to "tens" (RoCEv2) |

Invariants: one journal group per VM for life, node-affine; group commit; per-VM extents so trim is one pointer move; traffic classes on separate connections; TCP always works, RDMA is an accelerator; NVMe-oF rejected on semantics.

Quiz key: "No: the journal is sharded into hundreds of groups; each VM writes to its one assigned group."

### Lesson for the design

segstore scales the journal by giving every VM one fixed group and batching a node's VMs into one flush per round trip, because the per-append cost is the flush, not the bytes. For a two-host system "how the journal scales" reduces to two things. First, group commit: all VMs on a host share one mirror connection and one fdatasync per window, so the per-fsync cost is one RTT plus one flush regardless of VM count. Second, per-VM retention tails: keep each VM's journal entries in their own extents so one idle VM does not pin the whole log behind its untrimmed tail. Both are cheap and both are the difference between a journal that scales and one that does not.

The retention number is the one to copy. segstore's journal must hold "the cooling window plus destage lag" of fleet-class bytes. For the two-host system the local staging log must hold everything written since the last compaction plus everything the compactor has not yet confirmed placed at k copies; that sizes the staging device.

segstore keeps journal traffic on its own connections so a bulk shard transfer "never sits in front of an fsync". For a two-host system with one link, that means the mirror stream (if any) and the cold-read and chunk-placement streams should be separate TCP connections, and the mirror one should be small-write, latency-classed.

The RDMA framing applies directly to "RDMA as an experiment only": segstore's position is that RDMA is worth it for the journal first, is an accelerator over an always-working TCP path, and must not change the protocol's semantics (named segments, idempotent operations). The rejected alternative, NVMe-oF, is rejected because it exports block addresses rather than names. A research system that tries RDMA should keep the same message set over both transports so the comparison is transport-only.

The "10% of bytes are fleet class" figure is an assumption they flag as such. For a two-host system, the equivalent input is "what fraction of guest writes are followed by a flush", and it is unmeasured.

---

## Lesson 6. Raw devices

### Summary of the argument

"No filesystem anywhere: a superblock, a root ring, then extents or slots, on the PM1743 and the Exos alike. Every size above 4 KiB is ours."

"One rule for both media, superblock then root ring then units, and the unit differs because the work does: the PM1743 takes thousands of tiny fsync-critical appends a second, the Exos takes 32 MiB streams." The SSD holds "journal, metadata, warm cache. raw extents, no filesystem": per-VM journal chains (e1→e2→e3), a metadata journal, checkpoints, warm cache. The HDD holds "sealed segments as 32 MiB shards. raw slots, no filesystem" with a bitmap.

Recovery: "Both devices start the same way: a superblock, then a root ring. Recovery takes the newest valid slot of the ring, loads the checkpoint it names, replays the journal tail past it. Checkpoints are seconds to minutes apart, so the ring sees at most a million writes in a device lifetime, and the SSD remaps them anyway."

Units: "A drive exposes one unit to us, the 4 KiB block. Everything above it is a number we chose for a job." 16 MiB extent: "the SSD journal allocation unit sized for trim: a VM's retention tail is dropped one extent at a time, so the size sets how fine reclaim is · ours, in the SSD superblock. could be 8 or 64; the tradeoff is trim granularity against extent-table size". 32 MiB slot: "the HDD allocation unit equal to one shard, because a slot holds exactly one shard and a shard is 256 MiB ÷ 8 data pieces · derived from the segment size and the code, not chosen on its own". 256 MiB: "zone and segment · ours, the constant the rest of the ladder hangs off". "The two device units differ because they do different jobs: an extent is a queue segment that gets trimmed from the head, a slot is a fixed home for one shard."

Who maps what: "Three translations sit between a guest block and a flash cell. The daemon owns two; the third is the SSD's, and it is the one hidden layer this design keeps." Guest zone or LBA → root → (segment, base, length): "ours, per VM forks copy it; compaction rewrites it". Index + free bitmap → (drive, slot) or (SSD, extent): "ours, per device slot k = header + k × 32 MiB". LBA on a raw device: "O_DIRECT, no LVM, no dm, no filesystem". LBA → flash page: "inside the SSD, invisible · kept: commodity NVMe, not open-channel. we write large aligned sequential extents, name a placement handle on every write, and Deallocate every extent we free, so its collector has nothing to copy". "The drive owns only the final translation, and we steer it rather than address it: a handle on every write, a Deallocate on every free."

### Numbers, invariants, laws

| item | value |
|---|---|
| logical block | 4 KiB (the only hardware unit) |
| SSD journal extent | 16 MiB ("could be 8 or 64") |
| HDD slot | 32 MiB = 256 MiB ÷ 8 |
| segment | 256 MiB |
| checkpoint interval | seconds to minutes |
| root ring writes per device life | at most ~1 million |

Invariants: superblock, root ring, units, no filesystem; recovery is newest valid ring slot, load its checkpoint, replay the journal tail; O_DIRECT raw devices; two of three translations are the daemon's; Deallocate on every free.

Quiz key: "Both are ours: the slot is 256 MiB ÷ 8 data shards, the extent is a trim-granularity choice written in the superblock."

### Lesson for the design

segstore puts its journal on a raw device with a root ring and checkpoint-plus-replay recovery because the journal is the durability point and a filesystem under it would be a second hidden map and a second fsync path. For a two-host system whose write is "acked after local fdatasync", the fdatasync cost and tail latency depend on what is under the log. A log file on ext4 or XFS pays the filesystem's journal too. The lesson implies at least measuring fdatasync on a raw partition with O_DIRECT versus a file, and structuring the log as a chain of fixed-size extents so trim is dropping an extent from the head.

The extent-size argument is a trim-granularity argument: the finer the unit, the sooner the head of a VM's tail can be freed, at the cost of a bigger extent table. The two-host staging log's reclaim unit is the same knob.

The "who maps what" table is a good audit for the research design: guest LBA → per-VM map → (chunk hash) → per-host index → (device, offset) → drive. That is three daemon-owned translations rather than segstore's two, because chunk-level dedup inserts a hash lookup between the VM map and physical location. Each extra map is RAM and a consistency obligation.

---

## Lesson 7. Two maps

### Summary of the argument

"The PM1743 keeps a map of its own under ours." "The SSD keeps one entry per 4 KiB page, about 1 GB of DRAM per TB, because it writes out of place and collects erase blocks. Ours keeps one entry per 16 MiB extent. The drive's map is doing work our write pattern makes unnecessary, and modern NVMe offers three levels of doing something about it."

Deallocate: "when an extent's bit is cleared, tell the drive its LBA range is dead · mandatory. without it the drive believes trimmed journal bytes are live and copies them during its own collection: hidden write amplification on every trim". FDP: "one placement handle per write the drive groups data with the same handle into the same reclaim unit, so whole-extent frees leave it nothing mixed to copy · Flexible Data Placement, NVMe 2023. our SSD format requires it: a handle on every write, and open refuses a drive that lacks it". ZNS: "the drive does no collection and keeps almost no map; trim becomes a zone reset · zones are 1 to 2 GiB and device-chosen, drives are enterprise only, and the root ring needs one of the drive's conventional zones because it overwrites in place".

"We build for FDP only: Deallocate on every free plus a handle on every write, and nothing else. There is no plain-NVMe mode to keep correct, because every drive that will hold this format has FDP (next lesson). ZNS would be the same idea with the map deleted outright; it is what we do to guests one layer up, and no drive in the fleet has it. The HDD analogue is host-managed SMR, whose zones happen to be 256 MiB, one segment per zone."

What FDP buys: write amplification "about 1 reclaim units die whole, so the collector copies almost nothing; one host byte costs one flash byte · the drive stays at its raw write bandwidth after it fills". Endurance: "a mixed workload sits at 2 to 4, so the same drive lasts 2 to 4 times longer · matters for a journal that rewrites its whole capacity every few hours". Tail latency: "no background collection on the fsync path · collection competes with our appends for the same dies; with nothing to collect the millisecond spikes go away · the one that matters most here: an fsync outlier is a journal outlier". DRAM: "FDP: unchanged. ZNS: the map shrinks about a thousandfold".

### Numbers, invariants, laws

| item | value |
|---|---|
| SSD FTL map | 1 entry per 4 KiB page, ~1 GB DRAM per TB |
| daemon map | 1 entry per 16 MiB extent |
| write amplification with FDP | ~1 |
| endurance gain | 2–4× (mixed-workload WA of 2–4 removed) |
| ZNS zone size | 1–2 GiB, device-chosen |
| host-managed SMR zone | 256 MiB |

Invariants: Deallocate on every free is mandatory; a placement handle on every write; open refuses a non-FDP drive; no plain-NVMe fallback mode.

Quiz key: a freed extent is copied by the SSD's own GC "until we issue Deallocate for the range" (explanation: "Two maps disagree about what is live, and the lower one wins until told. Deallocate on every free is the minimum; FDP or ZNS removes the disagreement structurally.").

### Lesson for the design

segstore issues Deallocate on every freed extent and tags every write with a placement handle because otherwise the SSD's own collector copies dead journal bytes and injects millisecond spikes into fsync. For a two-host research system the transferable part is the minimum, not FDP: when the staging log frees an extent or the compactor frees a chunk region, issue a discard for that range. Without it, fdatasync tail latency on the staging device will degrade as the drive fills, and the measurement will blame the wrong layer. FDP itself does not apply unless the research hardware has it; the DGX Spark's NVMe is unmeasured on this point.

The "an fsync outlier is a journal outlier" line is the reason to care: if the write path acks after local fdatasync, its p99 is the drive's GC behaviour.

---

## Lesson 8. The fleet we have

### Summary of the argument

A hardware census, "Measured on the OVH fleet on 2026-09-01, read-only, with nvme-cli 2.16. The design above is written for this hardware, not for a catalogue." Compute boxes: "128 cores · 503 GB · kernel 7.1.4 · no HDD", "6 × PM1743 7.68 TB · data FDP", "2 × PM1743 1.92 TB · boot FDP", "today: md RAID10 over the six, ext4, 96% full". One variant has 6 × 15.36 TB ("92 TB of NVMe in one box, 80% full today"). Storage box: "72 cores · 125 GB · kernel 6.18 · Broadcom SAS38xx HBA", "36 × Exos X24 24 TB · data CMR · SAS", "2 × PM9A3 7.68 TB · journal front no FDP", "today: one ZFS raidz3 of 786 TB".

"Two shapes. Compute boxes are all NVMe and every drive in them has FDP. Storage boxes are HDD with a small NVMe front, and that front does not have FDP. The topology keeps the journal on the storage tier, so the front is a purchase: two FDP drives per storage box (a PM1743 or PM9D3a pair) before this ships."

PM1743: "FDP: yes, and off the controller advertises it (identify bit 19); 8 reclaim unit handles, 1 reclaim group, reclaim unit 16.6 GiB on the 7.68 and 15.36 TB parts, 8.3 GiB on the 1.92 TB". "ZNS: no." "Deallocate yes, Write Zeroes yes, Verify yes, Copy no. A 4 KiB logical block format exists (format 2) but the drives run 512e today". "Atomic write 4 KiB, optimal write 128 KiB, max transfer 256 KiB". PM9A3: "FDP: no. Directives: no. ZNS: no", "does not qualify: open refuses it". Exos X24: "CMR, not zoned", "512e over 4 KiB physical, 7200 rpm, SAS at 12 Gb/s, no UNMAP". Kernel: "7.1.4 on compute exposes FDP as block-layer write streams max_write_streams and write_stream_granularity exist in sysfs and read 0 while FDP is off · once enabled, a write names its handle in the io_uring SQE. no passthrough, no custom driver".

Design consequences. FDP only: "every SSD write names a placement handle, and open refuses a drive that cannot take one no probe-and-degrade, no second format, no flag in the superblock. a PM9A3 is an error at open, not a slower mode · one implementation to keep correct. the fallback would be the path nobody tests." Extent density: "1,062 journal extents share one reclaim unit the drive reports a reclaim unit of 17,817,403,392 bytes; divided by 16 MiB (16,777,216) that is 1,062. a handle fills reclaim units in time order and journal extents die in time order, retention later, so a unit empties almost as a whole · a slow VM pins a few extents in an old unit; the drive copies those and only those". Turning it on: "a reformat, not a setting FDPE is per endurance group and can only change while the group has no namespace: delete the namespace, set feature 1Dh, create it again (at most 2 under FDP). switch to 4Kn in the same step".

"Build for the drive we have, and only for it. A future ZNS purchase would be a new format, decided then; a future non-FDP drive is simply not a journal drive."

What the Exos can do: recovery time limit ("set it near one second: a bad read returns an error fast and the shard is rebuilt from parity instead of stalling the read"); depop not RMA ("a 24 TB drive keeps working at about 21 TB"); 4Kn at format time; outer tracks ("Low LBAs on a CMR drive stream about twice as fast as the inner ones"); write cache ("issuing one SYNCHRONIZE CACHE (the SCSI flush) per sealed shard is safe under the protocol, because a shard is only acked after that flush. Measure"); T10 PI skipped ("Our shard checksums already cover the path end to end").

Eight handles "assigned by when the bytes die rather than by what they are": 0 data journal, 1 metadata journal, 2 checkpoints ("replaced whole every few minutes"), 3 warm cache ("evicted whole segments; clean, so any unit can be dropped"), 4 local-class segments ("live for the VM's life, die together"), 5 superblock and root ring ("tiny and overwritten; kept out of every journal unit"), 6–7 spare.

Journal structure: "one chain of extents per VM in each group slice · records carry a sequence number and a checksum; the extent table is checkpointed · per-VM chains are what make trim one pointer move". Metadata journal and checkpoints: "the placement and refcount tables, appended then serialised whole · same record format as the data journal · one recovery discipline for both". Root ring: "a few slots naming the newest extent-table and metadata checkpoints, written round-robin with a sequence number and checksum". "Control-plane nodes use the same journal format on their own SSDs for the replicated log, and recover from those SSDs alone. Checkpoints may be archived into the segment pool as ordinary sealed segments, but the trust root never depends on the thing it manages."

### Numbers, invariants, laws

| item | value |
|---|---|
| PM1743 reclaim unit | 17,817,403,392 B (16.6 GiB); 8.3 GiB on 1.92 TB parts |
| journal extents per reclaim unit | 1,062 |
| FDP handles per namespace | 8 (6 used, 2 spare) |
| PM1743 atomic / optimal / max write | 4 KiB / 128 KiB / 256 KiB |
| PM9A3 max transfer | 2 MiB |
| Exos X24 | 24 TB CMR, 7200 rpm, SAS 12 Gb/s, 512e; ~21 TB after one-head depop |
| outer vs inner track streaming | ~2× |
| HDD error recovery limit | near 1 s (default tens of seconds) |

Invariants: FDP-only, no fallback mode; handles assigned by death time; journal records carry seq and checksum; one recovery discipline for data and metadata journals; the trust root recovers from its own SSDs only.

Quiz key: enabling FDP means "Drain the host, delete the namespace, set feature 1Dh, recreate the namespace, then write with handles".

### Lesson for the design

segstore builds for exactly the measured drives and refuses to keep an untested fallback path, because "the fallback would be the path nobody tests". For a two-host research system this implies doing the same census up front: what the two hosts' NVMe actually support (Deallocate, atomic write size, optimal write size, FDP or not) and writing those numbers into the design rather than assuming. The concrete numbers that shape a staging log are atomic write size (whether a record header and payload can be one atomic write) and optimal write size (the coalescing target for small guest appends).

"Assigned by when the bytes die rather than by what they are" is a placement principle that survives without FDP: keep the staging log, the chunk store, and the RAM-cache spill (if any) in separate regions or devices so that trims free whole regions.

The journal record format (sequence number plus checksum per record, extent table checkpointed, root ring naming the newest checkpoint) is the minimum for a crash-recoverable staging log and can be copied as is.

Most of this lesson (HDD mode pages, depop, outer tracks, T10) does not apply: no HDDs in the two-host system.

---

## Lesson 9. The control plane

### Summary of the argument

"Something decides which node runs a VM and holds every root. Its most important property is what it never touches."

It decides "fleet membership and health · which node runs a VM · journal group per VM · free space and load, published to daemons". It hosts "the root record service · replicated, sharded by family tree · fork, snapshot and migration commit here". It never "sees a byte of data · sits on a read or write path · is consulted per operation". "If the control plane is down, running VMs keep reading and writing: their daemons already know their journal group and their index. What stops is creating, forking, migrating, and repair scheduling." Quiz explanation for the wrong "writes stall" answer: "That would put a coordination service on the fsync path. The daemon already knows the VM's journal group and does not ask permission per write."

Sharded by family: "A fork touches two records: the parent snapshot's refcount and the new child root. Shard by VM id and those land on different consensus groups, so every one of the 333 forks a second becomes a two-shard commit, the exact class that leaks refcounts. So the unit is a subtree: a child is created in its parent's shard, and when a shard outgrows its budget a subtree is cut off to a new group with one forwarding pointer at the cut." The hot-spot worry is dismissed: "one Raft group commits 10,000+ a second with batching"; "Sharding the control plane is for blast radius and memory, not throughput." And: "Reads never touch consensus. A daemon resolves the chain from cached, immutable snapshot tables."

Cutting a subtree: "First the new group commits a copy of the subtree's records plus a pin on the parent snapshot at the cut edge. Then the old group commits a forwarding pointer at the cut, which is the single commit point: before it, the old copy is authoritative and the new one is unreferenced garbage; after it, lookups follow the pointer. A crash between the two leaves an unreferenced copy that the auditor collects." Cuts happen "every minute or so" at "tens of thousands of roots per shard and 333 births a second".

Which database: "Build the store, borrow the consensus. The state machine is the easy part: tables, an apply function, a checkpoint. The hard part of rolling your own is the consensus edge cases (membership change, snapshot transfer, pre-vote), so those come from a vetted Raft implementation driving our state machine." "recovery must complete from the control nodes' own SSDs, because the trust root cannot depend on the thing it manages." "etcd is a single Raft group whose MVCC compaction pauses would land in the fork path; Postgres failover is not consensus; FoundationDB is the one respectable external answer and the right pick for a team that has not already built a replicated journal."

### Numbers, invariants, laws

| item | value |
|---|---|
| fork rate | 333/s fleet-wide |
| one Raft group | 10,000+ commits/s with batching |
| shard budget | tens of thousands of roots |
| cut frequency | ~1/min |

Invariants: the control plane is never on the read or write path; a fork never spans two consensus groups; a cut has one commit point (the forwarding pointer); unreferenced copies are garbage the auditor collects; the trust root recovers from its own storage.

Quiz keys: control plane down means "Nothing: it keeps reading and writing, but nobody can fork, migrate, or create VMs". A 100,000-descendant family in one shard is not a hot spot.

### Lesson for the design

segstore keeps the control plane off the data path so that losing it costs only forks, creates and migrations, never an fsync. For a two-host research system this implies: whatever tracks "which host owns VM X" and "what is VM X's current root" must be consulted at VM start and at snapshot or fork, and never per write or per read. If the daemon needs a lookup on the write path to find its mirror target or its placement, that lookup must be a cached local decision made once at VM creation.

The two-record fork problem (parent refcount plus child root) is the same shape at any scale: if the per-VM map and the chunk refcounts live in different places, every snapshot is a two-place commit and refcounts drift. segstore's answer is to co-locate them and make the cross-place operation (the cut) a two-step with one commit point and an auditor for the half-done case. A two-host system with no consensus should at least have the auditor.

"Borrow the consensus" and the etcd/Postgres/FoundationDB comparison do not apply: two hosts cannot form a majority, so the research system has no consensus by construction. The honest statement is that its control state lives on one host at a time and the other host is a follower, and failover is a human or scripted decision, not an election.

---

## Lesson 10. A segment's life

### Summary of the argument

"A zone and a segment are the same size and not the same thing." A zone is "a 256 MiB slot in one VM's zoned disk, addressed by number · the guest appends into it, finishes it, resets it · a place; it spans 256 MiB of address space even when nothing is written". A segment is "the bytes on the host, at most 256 MiB, named by their hash at seal · shared by forks, moved by compaction, referenced by count · the thing itself". The map: "the root table: zone 7 → segment 9f3a…, offset 0, length 256 MiB · one entry per written zone, no per-block table · after a fork two VMs' zone 7 name the same segment; after compaction zone 7 names a new one". "Zone append, finish and reset in the guest are segment append, seal and delete on the host. The one place the sizes differ: a zone finished early (a snapshot cut it at 92 MiB) maps to a 92 MiB segment."

Lifecycle: open ("local NVMe, appending"), sealed and hashed ("immutable, content-named"), cooling on NVMe ("waiting to see if the VM dies"), EC destage to HDD ("only survivors get here"), reclaimed ("refs = 0"). "the ~99% that never leaves flash churn dies here and costs the HDDs nothing".

Quiz explanation for "how much reaches HDD": "Sealed segments wait on NVMe for the cooling window, a space budget sized to outlast the mean VM life (about 49 minutes at 340 MB/s into 1 TB of cooling space). Most VMs die first, their references drop to zero, and their segments are freed before any erasure coding or network transfer."

Four consequences of seal: "it gets a name · hash of its bytes, computed once at seal · any copy anywhere can be checked against it · identical content collapses to one segment for free". "it can be shared · forks reference it instead of copying · refcounts replace ownership". "it can be encoded · erasure coding needs final bytes · so EC happens after seal, after cooling · never on the write path". "it is no safer · sealing is about immutability, not durability · a sealed segment in cooling is exactly as safe as an open one · durability came from the journal, at ack time".

The interface:

```
trait Segments {
fn create(&mut self, class: Class, hints: Hints) -> SegmentId;  // open: addressed by place
fn append(&mut self, seg: SegmentId, bytes: &[u8]) -> u64;      // any length; byte offset back
fn seal(&mut self, seg: SegmentId) -> Hash;                      // frozen: addressed by content
fn read(&self, seg: SegmentRef, off: u64, len: u64) -> Bytes;   // SegmentRef = Id | Hash
fn delete(&mut self, seg: SegmentRef);                           // refcount -1; zero frees
fn probe(&self, hash: Hash) -> (Tier, Latency);                  // NVMe, warm SSD, or HDD
}
```

Is the compute NVMe a cache? "For fleet-class data, entirely: nothing on it is the only copy of anything. For local-class data it is storage, because those bytes exist nowhere else until they destage. That is the whole trade a local-class volume makes, and why it commits in 40 microseconds." Open segments: "dirty fleet: the journal holds the bytes · local: nowhere else · fleet: discardable · local: not". Cooling: "same split, until destage completes". Read cache: "copies of segments already on HDD · clean the authoritative copy is the EC shards · always discardable". "A write-back cache that loses its device loses data, which is why RAID controllers ship with batteries. Here the journal is the battery, off-board, so the cache stays discardable and the node stays disposable."

### Numbers, invariants, laws

| item | value |
|---|---|
| segment | at most 256 MiB; can be shorter if sealed early |
| cooling space example | 1 TB at 340 MB/s ≈ 49 min |
| bytes reaching HDD | ~1% |
| local-class commit | ~40 µs |

Invariants: zone is a place, segment is content; one root entry per written zone, no per-block table; seal gives a name, sharing, encodability, and no extra safety; EC never on the write path; compute NVMe is a cache for fleet class and storage for local class; the journal is the battery.

Quiz keys: two forked VMs' unwritten zone 7 is "One segment, referenced from both root tables". A 5-minute VM's data reaching HDD: "Roughly none". NVMe failure loses acked data for "None, unless a volume chose the local class".

### Lesson for the design

segstore separates immutability (seal) from durability (ack) and puts durability at ack time via the journal, so that the compute NVMe holding open and cooling segments is a discardable cache. For a two-host research system this is the crux of "ack locally or mirror before ack". Under local ack, the staging log is not a cache; it is the only copy, and the host is not disposable. Under mirror-before-ack, the staging log is a cache and the host is disposable. segstore's own words for the local-ack case: "That is the whole trade a local-class volume makes, and why it commits in 40 microseconds." The research design should state which it is, and if it offers both, it should measure the 40 µs versus 0.5 ms gap on its own hardware.

The cooling stage exists because ~99% of data dies before anyone pays for it. If the two-host workload is not short-lived VMs, cooling is dead weight; the compactor should chunk and place as soon as a staging extent is sealed. This is the second time the lessons force a measurement of VM lifetime before adopting a stage.

The "probe" call (which tier, what latency) is a small thing worth copying: a guest or a scheduler can ask where a chunk is before deciding to read it. For a two-host system with a RAM cache, local NVMe, and a remote peer, that is three tiers with very different latency.

---

## Lesson 11. Erasure coding

### Summary of the argument

"Destage splits a sealed 256 MiB segment into 8 data + 3 parity shards of 32 MiB on eleven distinct drives, on as many servers as the fleet has and never more than three on one server. Any 8 of the 11 rebuild the whole thing." Cost: raw 256 MiB; EC 8+3 352 MiB (1.375×); 3 full replicas 768 MiB (3×). "11 × 32 MiB = 352 MiB is the number every later lesson calls the HDD bill for one segment."

A 4 KiB read at offset 100 MiB: "100 ÷ 32 = slice 3 · so the bytes sit 4 MiB into shard k3 · index places k3 on one drive · one seek · 4 KiB of verbatim data back · no decoding, no other drive touched". "parity is a spare tire only a dead drive or a failed checksum makes the daemon fetch 8 survivors and solve for the missing slice". "Reading a whole segment does stream 8 drives in parallel, so the eight-drive intuition is real. It just belongs to big reads."

Naming is hash-first: "the hash names the logical segment, computed before encoding · shards inherit it: 9f3ac2…/k0, each with a checksum footer · the segment hash arbitrates reconstruction, scrub and restore". The code is per segment: "8+3 is the code for a fleet of four or more storage boxes · a smaller fleet needs a wider one, chosen at seal and recorded with the placement". Lifetime is segment-level: "dedup, refcounts and reclaim all act on the segment · parity shards have no independent lifetime · there is nothing to garbage-collect below a segment".

EC only on HDDs. Quiz explanation: "Sealed segments on HDD: EC 8+3. The journal and roots on storage-server SSD: 2–3 plain replicas. Compute NVMe: no protection at all, on purpose, because every fleet-acked byte there is already in the journal." And on the wrong "both" answer: "On the journal SSD, erasure-coding tiny latency-critical appends is pure overhead; small hot data wants plain replication."

Why 256 MiB: "Total HDD cost per stored byte (seek overhead plus index RAM plus reclaim amplification) is flat from about 128 to 512 MiB. The floor is seek amortisation (32 MiB shards keep seek overhead near 5%; 8 MiB shards would push it near 20% on every repair and scrub stream) and index size (about 4 million entries per petabyte at 256 MiB, four times that at 64 MiB). The ceiling is reclaim granularity: one live page pins a whole segment, which is the subject of the slow-leak lesson. Inside the plateau, 256 MiB is the zone size the zoned-Linux ecosystem is already tuned for. Bench 128 against 256 at bring-up; the constant is not what fixes the leak, the collectors are."

### Numbers, invariants, laws

| item | value |
|---|---|
| code | 8 data + 3 parity, 32 MiB shards, 11 drives |
| overhead | 1.375× (352 MiB per 256 MiB segment) |
| replicas alternative | 3× (768 MiB) |
| max shards per server | 3 |
| minimum fleet for 8+3 | 4 storage boxes |
| small read | 1 drive, 1 seek, no decode |
| seek overhead | ~5% at 32 MiB shards, ~20% at 8 MiB |
| index | ~4M entries per PB at 256 MiB; 4× at 64 MiB |
| cost plateau | flat from ~128 to ~512 MiB |
| journal and roots | 2–3 plain replicas |

Invariants: systematic code, data shards are literal slices; hash computed before encoding; shards inherit the segment hash and carry a checksum footer; parity has no independent lifetime; EC on HDD only, replication for small hot data, nothing on compute NVMe.

Quiz key: a 4 KiB cold read wakes "1: the shard whose slice contains the range". Recall answer: "The code is systematic: the eight data shards are literal slices of the segment."

### Lesson for the design

segstore erasure-codes only large, cold, immutable segments and replicates small hot data because EC on tiny latency-critical appends is "pure overhead". A two-host system cannot use EC in any meaningful way (any code needs more failure domains than two), so replication factor k is the only option, and this lesson says that is the right choice for the hot path anyway. It does not apply to the cold path either, for lack of hosts.

The 256 MiB argument is the answer to "segment vs chunk granularity", with its cost model stated: seek amortisation and index RAM push the unit up; reclaim granularity (one live page pins the whole unit) pushes it down; the plateau is 128 to 512 MiB on HDD. For a two-host NVMe system the seek term nearly vanishes, so the floor drops and the index term dominates: at chunk granularity of, say, 64 KiB, the index has 4,096× more entries per byte than segstore's. The lesson's own instruction transfers: "Bench 128 against 256 at bring-up; the constant is not what fixes the leak, the collectors are." Pick a granularity, but design the collector first.

Hash-before-encode and shards inheriting the logical name is the right pattern for any split: name the whole, derive the parts' names from it, so scrub and reconstruction have one arbiter.

---

## Lesson 12. Durability classes

### Summary of the argument

"The class never changes where bytes end up. It changes who waits, and for how long."

The path: guest write plus fsync → NVMe append on this compute node ("local returns ~40 µs") → journal confirms on storage server SSD ("fleet returns · default ~0.5 ms") → EC shards on HDD after cooling, in bulk ("archive returns seconds"). "local-class exposure window acked, but living on one node's flash. if the node dies here, these writes are gone." "two failure domains from here on node loss replays from the journal, so cooling and destage can take their time." "One write, three places it can be told done. The class picks which arrow returns to the application; everything to its right still happens, with nobody waiting."

"Class is chosen per volume on the block lane and per put on the CAS lane. Snapshot and pin force an upgrade: referenced bytes are promoted to fleet before the ack. A volume can also be declared ephemeral: local class, and never destaged, because it is declared to die with its VM. Block caches and scratch space live there. Snapshotting an ephemeral volume promotes it: the flag drops and its bytes are journaled before the snapshot acks."

Quiz explanations: the cost of a fleet-class SQLite commit is "One network round trip per fsync"; "Fleet class means an ack is not an ack until a storage server journals it: ~0.5 ms per commit. Choose local class and commits drop to ~40 µs, at the cost of losing the tail if the host dies." Losing a cooling fleet-class segment loses nothing: "The cooling copy is a staging copy. The journal replays onto another node, exactly like node death in the simulator. Cooling and durability run on different clocks on purpose." And: "There is no parity yet. EC happens at destage, and this segment has not destaged. The at-risk window is covered by the journal."

SQLite worked case: "page overwrites are absorbed as appends (log structure is good at churn), the WAL is a pure append stream, commits cost exactly the volume's class, and forks are the power-loss contract WAL recovery was built for. The one decision is the volume's class: how far must my fsync travel. The one cost that does not show up in latency is space: a long-lived database that keeps rewriting the same pages leaves mostly-dead segments behind."

### Numbers, invariants, laws

| class | ack point | latency |
|---|---|---|
| local | NVMe append on this node | ~40 µs |
| fleet (default) | journal confirms on storage-server SSD | ~0.5 ms |
| archive | EC shards on HDD | seconds |

Invariants: class changes who waits, not where bytes go; snapshot and pin promote referenced bytes to fleet before acking; ephemeral is local and never destaged; snapshotting ephemeral promotes it; the exposure window of local class is exactly "acked but on one node's flash".

Quiz keys: fleet-class SQLite pays "One network round trip per fsync"; a dying cooling device loses "Nothing".

### Lesson for the design

segstore answers "ack locally or mirror before ack" with a per-volume class and a default of mirror-before-ack, because the two differ by an order of magnitude in latency (40 µs vs 0.5 ms) and by one failure domain in exposure, and different volumes want different points on that line. For a two-host research system this is the most direct instruction in the fourteen lessons. The design's current "acked after local fdatasync, no network on the write path" is segstore's local class exactly, with segstore's own description of its exposure: "acked, but living on one node's flash. if the node dies here, these writes are gone." Offering a second class that waits for the peer's fdatasync before acking turns the same pipeline into fleet class. Both share one pipeline; only the return point moves.

Two rules attach to the local class and should be copied. First, snapshot promotes: a snapshot of a local-class volume must push referenced bytes to the peer before the snapshot acks, or the snapshot is not durable. Second, ephemeral volumes exist as a named class that is never replicated, for scratch and caches, which removes the largest write volume from the replication path with no loss of stated guarantees.

The "everything to its right still happens, with nobody waiting" property is what makes the compactor a background job in every class. This is also why the answer to "durability epochs" in lesson 13 is class-parameterised.

---

## Lesson 13. The durable watermark

### Summary of the argument

"Confirmations come back in any order. Two integers per VM turn that mess into fsync, snapshot, migration and trim." "Hundreds of appends are in flight and confirmations come back in whatever order they finish. Number every append per VM as it arrives. Now decide what fsync may say."

Definition: "E is the highest N such that appends 1 through N are all confirmed. If 7 is confirmed and 6 is still in flight, E sits at 5." "The ▲ watermark is the highest position with no unconfirmed append before it — 'durable through here' always means an unbroken prefix. A snapshot cut at the watermark is a state the disk really passed through; anything past it might have holes, and a holey history is a state that never existed." "Journaling filesystems and databases recover from the first kind of state. No recovery code on earth makes promises about the second. The watermark makes holey states unrepresentable."

Quiz explanations: reporting 7 is "Individually true and collectively useless: a disk state containing write 7 but not write 6 never existed at any instant". Tracking max-confirmed is "The classic bug. A max over confirmations forgets the gap, and write 6 is precisely the one still in flight. This is the answer that loses acknowledged data." Waiting for 6 is "Too strict. Everything through 5 is a durable prefix, so an fsync issued at write 3 can return right now. Only fsyncs waiting on 6 or later block."

"Add a second integer, D: everything at or below D has destaged to HDD. With E and D, every hard question becomes a comparison." fsync: "note the counter → wait until watermark ≥ it → return". Snapshot: "cut the open segments at E · the state is a real prefix by construction". Migration: "B fetches what has destaged (≤ D) from the storage tier → replays the journal (D, E] → A guarantees nothing past E was acked · two daemons agree on two numbers instead of a set". Journal trim: "drop this VM's entries ≤ D · recovery replays exactly (D, E] · per-VM extents make it one pointer move".

"an LSN, not a clock · databases call this a log sequence number: one writer, one log, total order · deliberately not a Lamport clock, which handles many independent writers · a Lamport clock can never give you a prefix". "a counter, not wall time · clocks cannot order two appends in the same microsecond · and they go backwards on NTP corrections · the counter advances exactly when the history does". "the class picks the event · local class advances on NVMe flush · fleet class advances on journal confirmation · same watermark, different definition of confirmed".

Recall answer: "It refuses to advance past a hole. Confirmations arrive out of order, so E only moves to the highest point with nothing missing behind it. That gives the prefix property. With the destaged pointer D beside it, snapshot cuts at E, trim drops up to D, and recovery replays exactly (D, E]."

### Numbers, invariants, laws

Two integers per VM: E (durable watermark, highest N with 1..N all confirmed) and D (destaged watermark, everything ≤ D on HDD). D ≤ E.

Invariants: E never jumps a hole; fsync waits for E ≥ its own sequence; snapshot cuts at E; migration transfers (≤ D) from storage and replays (D, E]; trim drops ≤ D; recovery replays exactly (D, E]; E is a per-VM LSN, not a clock; the class defines "confirmed".

Quiz key: "Durable through 5, the last point with nothing missing behind it."

### Lesson for the design

segstore reduces durability tracking to two per-VM integers and forbids the watermark from skipping a gap, because a max-over-confirmations reports states that never existed and "is the answer that loses acknowledged data". For a two-host research system this is the answer to "how durability epochs/watermarks should work", and it is complete enough to implement from:

- Number every append per VM at arrival (a per-VM LSN).
- Keep E as the highest contiguous confirmed prefix. Under local class, "confirmed" is the local fdatasync; under mirrored class, it is the peer's confirmation. The same E serves both.
- Keep D as the highest prefix the compactor has chunked and placed at k copies. Trim the staging log at D. Recovery, migration and peer catch-up replay exactly (D, E].
- Snapshots cut at E, never at "latest".

The migration line is worth quoting because it is also the two-host failover procedure: "B fetches what has destaged (≤ D) from the storage tier → replays the journal (D, E] → A guarantees nothing past E was acked". In a two-host system the "storage tier" is the peer's chunk store and the journal (D, E] is the staging log, which under local class exists only on the dead host. That is the concrete statement of what local ack loses: exactly the range (D, E] at the moment of death.

The warning against clocks applies to any design that considered timestamping writes for ordering across two hosts.

---

## Lesson 14. What the guest sees

### Summary of the argument

"Two disks and a directory. One of the disks is a lie the guest is allowed to believe, and the guest never learns the word segment." The guest sees "/dev/vda · plain ESP · ext4 root · XFS log and metadata 8 GiB", "/dev/vdb · zoned zoned XFS data 128 TiB · 524,288 zones", "/cas virtio-fs + vsock nix · jj · container layers", and optionally "raw segments io_uring passthrough optional: LSM stores". Underneath: "segstore daemon vhost-user and virtio-fs, one root record per VM". "segstore is a host daemon, not a device. The guest sees ordinary virtio."

The zoned lane maps one-to-one: "a zone on /dev/vdb VIRTIO_BLK_F_ZONED · 256 MiB each, both sides"; "append(seg, bytes) → offset · REQ_OP_ZONE_APPEND returns the landing LBA · the guest never picks an address"; "seal(seg) → hash · delete(seg) · zone finish · zone reset · a reset is a decref, not an erase"; "active zones per VM · max_active_zones · single digits, for flash budget". Four guest patches: "death and read groups · per-write hint plumbing; event virtqueue → uevents".

Four ways in: zoned XFS ("the default for everything · XFS turns writes into zone appends and runs zone GC"), plain disk ("the host keeps an LBA map and repacks · boot, ext4, the XFS log"), /cas ("immutable blobs by name · shared across forks and tenants in a domain"), raw segments ("exactly the interface the host uses internally, over io_uring commands · for software whose files are already immutable, written once and deleted whole: SSTables, log chunks"). "Raw segments are not a different storage system. They are the same segments, the same root record, the same refcounts, with the zone and filesystem layers removed for a program that would only have fought them."

The whole API, per lane. Zoned: "read · zone append returns the landing LBA · zone finish seals; the segment gets its name · zone reset the ONLY delete: drops the whole 256 MiB · report zones write pointers and states · no write-in-place, no discard, no per-block anything". Plain: "read · write becomes an append plus an LBA-map update · discard removes the LBA's map entry: a real per-block delete, because the host owns this map · flush waits for the volume's durability class · the 8 GiB compatibility view". /cas: "get open('/cas/<hash>'), read or mmap · put bytes in, the host computes the name · pin · unpin the reference; unpin is the delete · have is it reachable from my roots and pins · readdir is empty on purpose". Hints: "death group · read group · durability class per volume or per put · advice, never required". Events: "pressure · quota · zones the host would like reset by number, emptiest first · arrive as uevents; an unpatched guest is deaf to them".

"Four verbs on the zoned disk and none of them is delete. Deleting is something XFS does to itself, and a zone reset is how the host finds out. Append is variable-length everywhere: the block lanes always send whole 4 KiB blocks, /cas and passthrough send any length, and the daemon coalesces small appends into aligned flash writes."

The typed interface (abridged; the full listing is in the lesson):

```
struct Zone(u32);              // a place on ONE vm's zoned disk
struct Lba(u64);               // a place on the plain disk, or inside a zone
struct SegmentId(u64);         // an OPEN segment: addressed by place (node, id)
struct Hash([u8; 32]);         // a SEALED segment or a blob: addressed by content
struct Name { domain: Domain, hash: Hash }
enum Class { Local, Fleet, Archive }   // how far an ack travels
type Block = [u8; 4096];

trait PlainDisk {
fn read(&self, at: Lba, n: u16) -> Vec<Block>;
fn write(&mut self, at: Lba, blocks: &[Block]);   // becomes append + map update
fn discard(&mut self, at: Lba, n: u32);          // a real per-block delete: map entry removed
fn flush(&mut self) -> Durable;                  // waits for the volume's Class
}

struct Root { parent: Option<SnapshotId>, zones: BTreeMap<Zone, Extent> }
struct Extent { seg: Hash, base: u64, len: u64 }   // zone -> segment, base offset, length
```

Why two disks: "XFS keeps its log and metadata on a conventional device and puts file data in zones; the data disk is the realtime device". Quiz explanation: "Zoned XFS writes data with zone append, but its log and B-trees still need in-place writes. So vda carries them through the conventional view and vdb is the zoned realtime device. Both live under one root record, so a fork captures both." Setup: `mkfs.xfs -f -m rtinherit=1 -r rtdev=/dev/vdb /dev/vda3` and `mount -o rtdev=/dev/vdb /dev/vda3 /data`.

The small disk is a view: "The boot disk needs in-place writes, and nothing here overwrites. So /dev/vda is a view: a small, visible translation layer over ordinary segments." "ext4 writes LBA 5000 · believes it overwrote in place · LBA map in the daemon 5000 → segment 91, offset 3.2 MiB · append into an open segment · the previous version becomes garbage". "An LBA that was never written has no map entry, so it reads as zeros the daemon synthesises. The provisioning leak cannot be rebuilt even inside the compatibility layer." "Two facts about this lane matter later. It is about 8 GiB (the ESP, the ext4 root, and the XFS log and metadata), so its churn is bounded. And the host owns its map, so the host knows exactly which bytes inside a segment are still live. Neither is true of the zoned data disk, and the difference decides who collects garbage on each lane."

Deleting a file on the zoned disk: the host learns "Nothing yet. XFS marks the blocks free in its own metadata; the host finds out only when XFS evacuates the zone's survivors and resets it". Quiz explanation: "Until then the segment stays fully referenced and fully billed, which is why the zone GC trigger patch matters."

### Numbers, invariants, laws

| item | value |
|---|---|
| plain disk | ~8 GiB (ESP, ext4 root, XFS log and metadata) |
| zoned disk | 128 TiB, 524,288 zones of 256 MiB |
| block | 4 KiB |
| active zones per VM | single digits |
| guest patches | four (death group hint, read group hint, event virtqueue, zone GC trigger) |

Invariants: the guest never picks an address on the zoned lane; zone reset is the only delete there and it is a decref; on the plain lane the host owns the LBA map, so discard is a real per-block delete; unwritten LBAs read as synthesised zeros; flush waits for the volume's class; hints are advice, never required; an unpatched guest boots; forbidden and not-found are the same value on /cas.

Quiz keys: deleting a file on the zoned disk tells the host "Nothing yet"; two disks because XFS needs a conventional device for log and metadata; ext4 on the plain disk does not break law 1.

### Lesson for the design

segstore gives most guests a zoned disk so the guest's filesystem does the block-level garbage collection and the host only ever sees whole-segment appends, seals and resets; it keeps a small plain disk as a view only because ext4 and the XFS log need in-place writes. For a two-host research system on stock QEMU with vhost-user-blk, the plain-disk lane is what the design already is: every guest write becomes an append plus an LBA-map update, discard is a real per-block delete, flush waits for the class, unwritten LBAs read as zeros. The lessons' two facts about that lane are then the design's facts: the host owns the map, so it knows exactly which bytes are live and must run the collector itself; and the lane's churn is bounded only if the disk is small. At 128 TiB of plain-view disk, the host carries a per-block map and the whole collection burden that segstore hands to XFS.

The zoned lane is exposed to the guest as VIRTIO_BLK_F_ZONED with zoned XFS on the guest side; the lesson calls this "ordinary virtio". Whether the research system's QEMU and vhost-user-blk daemon support the zoned feature is not something the lesson addresses and is unverified here. What is not stock is the four guest patches, and the lesson is explicit that "an unpatched guest still boots" without them. So "what the guest should see" has a stock answer and a patched answer. The stock answer is a plain disk with discard and flush, plus optionally a zoned disk with zoned XFS. The patched answer adds death-group and read-group hints and pressure and reset-request events. For a research system the plain disk is the baseline and the zoned disk is the experiment that offloads collection to the guest.

"Append is variable-length everywhere ... the daemon coalesces small appends into aligned flash writes" is the staging log's write path in one sentence.

The /cas lane and the io_uring passthrough do not apply to a block-only research system.

---

## Cross-cutting: what changes for the two-host design

Collected from the per-lesson sections, ordered by how much they bear on the five open questions.

**Ack locally or mirror before ack.** segstore does both, per volume, and calls them local and fleet class (lessons 2, 10, 12). Local-ack is a named class with a stated exposure ("acked, but living on one node's flash. if the node dies here, these writes are gone") and a stated benefit (~40 µs vs ~0.5 ms). The default is fleet. Two rules attach: snapshot promotes referenced bytes to fleet before it acks (lesson 12), and a node under local class is not disposable (lessons 3, 10). For the research system the design-changing move is to make the class a volume property and measure both, rather than choosing one.

**Durability epochs and watermarks.** Two integers per VM (lesson 13): E is the highest contiguous confirmed prefix and never skips a hole; D is the highest prefix that has been destaged. fsync waits for E; snapshot cuts at E; trim drops ≤ D; recovery and migration replay (D, E]. The class chooses what "confirmed" means, so one mechanism serves local and mirrored volumes. This transfers to the research system without change; the compactor's placement confirmation defines D.

**Segment vs chunk granularity.** segstore names, refcounts and deletes at 256 MiB and never below (lessons 3, 11), accepting the slow leak because one unit makes fork and reclaim O(1). The cost model is seek amortisation and index RAM against reclaim granularity, flat from 128 to 512 MiB on HDD. On NVMe the seek term is small, so the index term dominates and the floor drops, but "the constant is not what fixes the leak, the collectors are". Naming is hash-first with parts inheriting the whole's name (lesson 11). Durability and naming are separate axes (lesson 3): bytes can be mirrored raw before they are hashed.

**What the guest should see.** On stock QEMU, the plain disk lane (lesson 14): write becomes append plus map update, discard removes a map entry, flush waits for the class, unwritten LBAs read as zeros. The host then owns the map and the collector. The zoned lane with zoned XFS is the option that moves collection into the guest, and it needs no host-side patches to boot, only the four patches for hints and events.

**How the journal scales.** One fixed group per VM, node-affine, with group commit so all of a host's VMs share one flush per round trip; per-VM extents so trim is one pointer move and a slow VM cannot pin the group (lesson 5). Journal traffic on its own latency-classed connections. Retention sized to cooling plus destage lag. RDMA is an accelerator for the journal over an always-working TCP path and must not change the protocol's semantics; NVMe-oF was rejected for exporting addresses instead of names.

**Things that do not transfer.** Erasure coding (needs more failure domains than two). Consensus and the control plane's sharding (needs three boxes). The "Kill any box" availability promise (needs four). FDP handles (hardware-dependent and unmeasured on the research hosts). The /cas lane, domains, and forbidden-equals-not-found (multi-tenant). HDD tuning.

**The largest contrast.** segstore's one deliberately missing link is compute-to-compute (lesson 4), because pooling node-local flash "rebuilds a storage fabric, couples failure domains, and tempts everyone to treat cache as durable". The two-host research system is that link. The lessons do not say it cannot work; they say a node's RAM and flash may be "a cache, never a source" for any byte that has been acked at fleet class, and a design that counts a peer's RAM cache toward k has broken that rule.
