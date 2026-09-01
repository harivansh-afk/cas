# segstore lessons 15 to 28: what they say and what they imply for a two-host CAS block backend

Source: https://internal.indexable.workers.dev/segstore/ (lessons 15 through 28), fetched 2026-09-01. Full page text was pulled with curl and converted with pandoc; quiz answer keys are not in the static HTML, so where a "correct answer" is stated below it is inferred from the lesson body and marked as such.

Reference design under review (ours): vhost-user-blk daemon on stock QEMU, local append-only staging log (ack after local fdatasync, no network on the write path), background compactor that chunks, hashes and dedups into a chunk store, chunks placed across two hosts by rendezvous hash with replication factor k, per-host hash-keyed RAM cache, cold remote reads over TCP (RDMA as experiment), migration by moving the offset-to-hash map, mark-and-sweep GC with epochs. Study: (1) single-host parity with ZFS dedup, (2) cross-host transfer and capacity wins, (3) cost of a remote cold read per transport.

Vocabulary from earlier segstore lessons that these pages assume: a **segment** is a 256 MiB append-only unit, content-named at seal; **cooling** is the window on compute NVMe between seal and destage to HDD; **fleet class** volumes journal every acked byte to a replicated journal on storage servers, **local class** volumes do not and die with the host; **zones** are what the guest addresses (zoned block device, one zone maps to one segment); **root record** is a VM's per-disk pointer plus delta table; **law 3** is "nothing on compute NVMe is the only durable copy".

---

## 15. The /cas lane

Lede: "Fifty forks read the same layer and it lives in RAM once. A 200-byte blob and a 3 GB layer land in the same kind of segment."

### Design and reasoning

The /cas lane is a virtio-fs mount whose namespace is hashes. "The guest calls open("/cas/9f3ac2…"). Mapping is the host's decision, and READDIR on /cas is empty on purpose." CAS-native software (nix, jj, container layers) asks for blobs by name over virtio-fs; `put`, `pin` and `have()` ride a vsock side channel. "The host always computes the hash: a guest-supplied name is never trusted."

Four properties of the mapping:

- **read-only, always.** "blobs are immutable, so mappings are PROT_READ · no writable page is ever shared between VMs. no tenant can change bytes another is reading"
- **per-VM everything.** "one virtiofsd per VM, with that VM's tenant identity · its own fixed-size window. a window holds only what this VM asked for and was allowed"
- **confined to a domain.** "two VMs share a physical page only inside one tenant, or inside a declared public domain · the same boundary that governs disk dedup. shared pages across distrusting tenants are a side channel"
- **a mapping is a reference.** "reclaim cannot free a mapped blob · revocation tears the mapping down. nothing dangles"

"Mapping is not a second way in. It rides the same reachability check as get, applied at open time: a blob you may not read is a blob you may not map."

Physical layout: a blob is arbitrary length; everything underneath is fixed-size. Blobs pack into segments ("small blobs share one; a large blob spans several"), segments seal and erasure-code as 256 MiB into 8 data plus 3 parity, each shard lands in a 32 MiB slot on raw HDD with no filesystem. "The index carries both mappings: blob hash → the segment extents holding it, and segment hash → the drives and slots holding its shards."

Packing rule: "a 200-byte blob is appended into the segment currently open for its (domain, class) pair, so a segment never mixes tenants, and reading it back is one index lookup and a range read inside a shard. Git packfiles and S3 extents do the same." A 3 GB layer "spans twelve segments, thirteen if it starts partway into one, and the index holds the ordered list."

Allocator: "Each HDD is a superblock plus a grid of 32 MiB slots with a free-slot bitmap. Writing a shard is picking a free bit and streaming; reclaim clears bits." "no best-fit search, no compaction, no gap too small to reuse. Every shard is the same size, so every free slot fits every shard. That is the whole allocator."

Cost of fixed slots: "a segment sealed early, say by a snapshot cutting it at 92 MiB, produces short shards that still occupy full slots. Compaction in cooling consolidates short segments before they destage, so the waste is bounded by how often a short segment survives its cooling window."

### Numbers, invariants, failure modes

| item | value |
|---|---|
| segment size | 256 MiB |
| EC code | 8 data + 3 parity |
| shard / slot size | 32 MiB |
| 3 GB layer | 12 segments, 13 if unaligned |
| fragmentation in slot grid | always 0% (interactive widget caption) |

Invariants: host computes every hash; mappings are PROT_READ; a mapped blob cannot be reclaimed; one virtiofsd per VM; a segment holds one (domain, class) only; reachability is checked at open, identical to get.

Quiz (inferred correct): VM 51 is stopped because "the mapping request goes to the host daemon, which applies the same reachability check as an ordinary read." A 200-byte blob costs "appended into an already-open segment; the index records its offset and length."

### Lesson for our design

segstore separates the *blob* (variable length, hash-named) from the *segment* (fixed, the unit of placement, EC and reclaim) because HDD economics, the allocator, and refcount table size all want a large fixed unit while software wants arbitrary-length names. For a two-host research system this implies: our chunk store should also have two layers of naming. The offset-to-hash map names chunks; chunks should be packed into a larger fixed-size container that is the unit of placement, replication and GC, with a chunk index of (container, offset, length). Placing individual 4 to 64 KiB chunks by rendezvous hash gives you one placement record and one refcount per chunk, which is the "billion 200-byte blobs would rival the data" problem from lesson 22. segstore's fixed-slot bitmap allocator is not needed at our scale, but the *reason* for it (every unit the same size means no fragmentation, no best-fit) is worth copying at container granularity.

The /cas lane itself (virtio-fs, DAX, vsock put/pin/have) does not apply to a vhost-user-blk backend: our guests see a block device, not a hash namespace. It matters only if the study wants to show RAM sharing across VMs (see lesson 16).

---

## 16. The DAX sharing boundary

Lede: "Sharing one physical page between fifty VMs is exactly what got KSM banned from clouds. Here is why we do it anyway, and where the line is."

### Design and reasoning

KSM (2009) scans memory for identical pages and merges them copy-on-write. The attack chain against it:

1. "Fill a page of your own memory with a guess at what a neighbour holds."
2. "Wait for the scanner. If somebody on the host has an identical page, the two are merged into one frame."
3. "Write to your page and time it. A merged page takes a copy-on-write fault, which is measurably slower."
4. "Slow means merged means somebody here has exactly these bytes." (used to fingerprint programs and break ASLR from a browser tab)

Then Rowhammer: "Copy-on-write stops software writes, not physics." Five-step chain: template a page with a reliably flippable bit; predict victim content (a distro public key); write it so the scanner merges; hammer; "the victim maps that frame, so their key now has your flipped bit; choose the flip so the modulus factors." ECC is treated as "a mitigation, not an answer" (inferred correct quiz answer: "No: it raises the cost and was then bypassed").

"DAX sharing is deduplication. Fifty forks reading one blob through one physical page is precisely the condition those attacks need. What makes us safe is the boundary." Three boundary properties:

- **domain-confined.** "a page is shared only inside one tenant, or inside content public by declaration · two distrusting tenants never share a page. step 3 of the attack has no move to make"
- **read-only against immutable blobs.** "no copy-on-write fault exists · so there is nothing to time. kills the leak channel outright"
- **sharing is requested, not discovered.** "an explicit authorised request maps the page · no scanner ever confirms that a guess matched. removes the guess-and-check loop"

Limit of hashing: "Content addressing does not catch a flipped bit in a mapped DRAM page: storage scrub reads the stored copy, which is intact. The defence is the boundary above, not the hash. What the hash does buy is that the corruption cannot propagate: a re-read from storage is verified against the name."

Residual risk in a shared public domain (inferred correct quiz answer): "Side channels: a shared physical page leaks access timing between the tenants sharing it."

**Reclaim order under memory pressure** ("the part people get backwards"):

1. "shared /cas pages go first. clean, file-backed, one LRU across every guest on the host; the host kernel already reclaims these before anything else"
2. "then guests drop their own page cache. the balloon inflates, the guest kernel reclaims clean cache before it swaps; the host asked, the guest chose what"
3. "guest-owned memory last. each VM sits in a cgroup with a memory.low floor; only past both steps above does anything a guest owns get compressed or swapped"

Dedup: "a /cas blob is host page cache, mapped straight into every guest that opens it. the guest kernel keeps no copy of its own: DAX means no guest page cache for that file. fifty VMs loading the same 20 GB of model weights hold 20 GB on the host, once".

The lesson says block lanes cannot do this. "a guest page cache for /dev/vda or the zoned disk is guest RAM, invisible and private. the host cannot share or prune it; the balloon is the only lever, and inflating it makes the guest drop clean page cache before it swaps. so shared content belongs in /cas, not on a block device. a base image read through virtio-blk is duplicated per VM".

"not KSM: scanning guest RAM for identical pages and merging them costs a core per host and re-creates the cross-tenant Rowhammer aim this lesson opened with. sharing is by name and domain, decided at open, never discovered by scanning."

Closing quiz (inferred correct): ten VMs mounting a 20 GB dataset read-only from a shared block volume hold "Ten: one per guest page cache, and the host cannot see or prune any of them."

### Numbers, invariants, failure modes

| item | value |
|---|---|
| KSM cost | "a core per host" |
| model weights example | 50 VMs, 20 GB, held once on host |
| copies via shared block volume, 10 VMs | 10 (guest page caches) |

Failure modes: KSM timing side channel; Rowhammer through a merged frame; ECC bypassed; timing side channel persists even inside a permitted domain.

### Lesson for our design

segstore gets one-copy-per-host RAM sharing by making the shared content a *file* mapped DAX into each guest, because a block device's page cache is guest-private and the host cannot see it. **This does not apply to a vhost-user-blk backend, and the lesson says so directly: "a base image read through virtio-blk is duplicated per VM."** Our per-host hash-keyed RAM cache dedups the *daemon's* copy, but each guest still holds its own page cache of the same bytes. For the study this means: any "RAM savings" claim must be scoped to the daemon cache, not host RAM overall, and a measurement of host RSS across N VMs booting one image will show N guest page caches plus one daemon cache. If cross-VM page sharing is a result the study wants, it needs virtio-fs with DAX (or virtio-pmem) for the base image, which is a different lane from the block device.

The security boundary applies regardless: our cache should never scan for identical guest pages (KSM), and if two VMs on the same host belong to distrusting tenants, dedup across them is a timing oracle even inside the daemon. For a research system with one tenant this is a stated assumption, not a mechanism.

---

## 17. Names, secrets, and keys

Lede: "You know the hash of another tenant's file. What does that get you, and what does encryption change?"

### Design and reasoning

"A hash is a name, not a capability" is the lesson's principle. Flow: tenant B knows a hash "from a lockfile, a log, a public index" → "reachability check: is this hash reachable from B's own roots and pins?" → "yes → bytes. B already had a claim on this content" / "no → ENOENT. byte-identical to 'never existed'".

Why: "hashes travel without content. lockfiles, git trees and SBOMs broadcast names of bytes you may not hold. so the name cannot be the key." And "low-entropy content: enumerate guesses, hash them, probe · a store that answers differently is a confirmation oracle. so the refusal must equal absence."

The one exception: "public-artifact domains: distro layers, binary caches · may treat hash as capability: the content is public by declaration. the one place hash-as-key is fine." Everywhere else: "tenant-private dedup domains · cross-tenant dedup off. default, not opt-in."

Encryption at rest: "Content addressing and encryption pull in opposite directions: identical plaintext must get one name for dedup to work, and identical plaintext must not get one ciphertext across tenants or the ciphertext is the oracle. The dedup domain already draws the line, so the keys follow it."

"Name by the hash of the plaintext. Encrypt with the domain's key." Consequences:

- segment on HDD: "encrypted under the key of the domain that owns it · a segment holds one domain's data only. the natural unit, since packing is already per class"
- the name: "hash of the plaintext, as before; the index key is (domain, hash) · scrub verifies after decrypting. a bare hash means something only inside a domain"
- the journal: "same key as the volume's domain. acked bytes are never plaintext off the node"
- key loss: "a domain's data is gone · which is the same as delete. crypto-shredding falls out for free"

Quiz (inferred correct): two tenants uploading the same 3 GB layer into private domains land "Two, one per domain, because the segments are encrypted under different keys."

### Numbers, invariants, failure modes

Invariants: index key is (domain, hash), never bare hash; refusal is byte-identical to absence; cross-tenant dedup is off by default; a segment holds one domain.

Failure modes: confirmation oracle via differing responses; ciphertext-equality oracle if encryption is convergent across tenants; hash-as-capability leaks via lockfiles and SBOMs.

### Lesson for our design

segstore gates every hash lookup on reachability from the caller's roots because hashes leak freely and a store that answers "have(h)" honestly is an oracle for low-entropy content. For our design: `have()` between the two hosts is a daemon-to-daemon protocol, not a guest-facing one, so the oracle is closed by construction as long as guests cannot issue hash queries. The point that bites is the dedup domain. If the study runs VMs from multiple "tenants" and dedups across them, the timing of a write that dedups (hash hit, no data transfer) versus one that does not is observable from the guest. segstore's answer is that cross-tenant dedup is off by default and the domain is part of the key. For a single-tenant research system, state "one dedup domain" as an explicit assumption in the paper and note that multi-tenant deployment needs (domain, hash) keys. Encryption does not apply unless the study claims at-rest confidentiality; if it does, name by plaintext hash and encrypt per domain, and note that it kills cross-domain dedup by design.

---

## 18. Capacity and quotas

Lede: "Twelve exabytes promised over a few petabytes owned. Hitting the wall is a designed code path, not an accident."

### Design and reasoning

The pressure ladder:

1. "~70% full: garbage collection turns aggressive. the auditor sweeps, half-dead segments are repacked. nobody notices."
2. "~80% full: stop admitting new VMs and forks. provisioning notices."
3. "~90% full: refuse new segments for tenants over quota. a clean out-of-space error on the write, for the overspenders and only them."
4. "~95% full: refuse all appends. every writer, loudly and immediately."
5. "the reserve is never usable capacity. GC scratch plus a repair reserve of at least m drives: a pool that cannot heal turns the next failure into data loss."

"Reads and deletes never fail for space. Deleting requires no allocation, so it is the last operation standing. Full-pool is a first-class path: HDD refusal, then a cooling backlog, then the NVMe watermark, then a clean guest write error. Nothing buffers hopefully."

"Sizes are promises. Quotas meter what exists. Usage is the segments reachable from a tenant's roots and pins. An unwritten zone is a table entry and costs nothing."

- "snapshots count: a snapshot pins segments · so deleting a file does not shrink usage while a snapshot holds it. the surprise most tenants meet first"
- "shared segments charge everyone: each referencer is billed in full. dedup is provider margin, not tenant discount"
- "tenants never contend for devices: quota is aggregate · 'someone filled my disks' has no referent. placement is the provider's problem"

Quiz (inferred correct): a tenant deletes 2 TB and usage does not move because "A snapshot or fork still references the segments those files lived in."

### Numbers, invariants, failure modes

| threshold | action |
|---|---|
| ~70% | aggressive GC and repack |
| ~80% | no new VMs or forks |
| ~90% | refuse new segments to over-quota tenants |
| ~95% | refuse all appends |
| reserve | GC scratch + at least m (parity count) drives |

Invariants: reads and deletes never fail for space; a delete needs no allocation; the full-pool path propagates back to a clean guest write error rather than buffering.

### Lesson for our design

segstore makes full-pool a designed path with a ladder because a thin-provisioned system will hit the wall, and a system that "buffers hopefully" loses data. For our design: the local staging log is exactly a place that "buffers hopefully" if the compactor falls behind or the remote host refuses chunks. Define the back-pressure chain explicitly: chunk store full → compactor stalls → staging log hits a watermark → daemon returns ENOSPC to the guest write. Also copy the invariant that deletes (discards, in our case) never need allocation, which constrains how the offset-to-hash map records a discard (a tombstone in the log is an append, so make sure the log reserves room for it). The billing points (snapshots pin, each referencer billed in full) are the correct way to report "capacity wins" in the study: count reachable chunks per VM root, and report dedup as the difference between the sum of per-VM reachable sets and the union.

---

## 19. Forks and snapshots

Lede: "Snapshot a terabyte. Fork it 333 times a second. Stack ten thousand of them. Guess the cost of each before reading on."

### Design and reasoning

"Only two things about a VM's storage ever change: its root record, and the write positions of a few open segments. Everything else is sealed. So each of these is cheap."

- **snapshot:** "cut open segments at their current length, save the root under a name · the root already holds its segment refs and one parent ref; nothing per segment. microseconds, no data copied"
- **fork:** "a snapshot both sides keep writing after · one reference on the parent snapshot. same cost whatever the image size"
- **revert:** "swap the live root back to a saved one. one pointer assignment"
- **delete:** "drop references · bytes are reclaimed when a count hits zero. the only thing that frees space"

"The contract is crash-consistent: a snapshot equals the disk at power loss, the state journaling filesystems and databases already recover from. EBS ships the same contract; a guest-agent freeze is an optional upgrade."

Widget example: at a fork, "s3 was open at fork time, so it is virtually sealed at 92 MiB; each side appends into a segment of its own after it." "No segment count moved. The fork froze the parent's delta into snap-9, pointed both live roots at it, and took one reference on snap-9 (now 2)."

**Ten thousand deep.** "A flat root (zone → segment list) would make each fork of a large image a refcount walk: millions of deltas a second at our spawn rate. So a root is a pointer plus a delta, and reads resolve through the chain. The requirement says that chain can be ten thousand long."

- "a root is a pointer + a delta: parent-snapshot pointer plus its own zone table. fork = one refcount + one root write. group-committed, sub-millisecond"
- "reads resolve through the chain: child first, then ancestors"
- "every ~32 levels, a snapshot is flattened into a skip node: a fully merged zone table, and no parent pointer. a read stops at the nearest skip node. so a 10,000-deep chain costs at most ~32 hops"

"Flattening is not an optimisation here; it is what keeps read cost independent of ancestry. A flatten copies a table of names, never a byte of data."

"A skip node holds direct references on every segment it names and no reference on its parent. One rule, three jobs: reads stop there, delete cascades stop there, and the chain above it is collectible on its own."

- "tables stay out of RAM: a full table for a written-out 128 TiB disk is ~24 MiB (16 B per zone plus 32 B per distinct segment) · so a flattened table is stored as a segment, content-addressed, and the skip node points at it. identical tables dedupe; roots stay kilobytes"
- "the protocol is increment-early: take direct refs on every segment → commit the merged table → then drop the parent ref. a crash in between leaks, never frees live data; at ~10 flattens/s fleet-wide the auditor heals it"
- "deltas carry tombstones: a child that resets a zone it inherited records 'zone Z: empty' · otherwise the read falls through to the parent and deleted data comes back. flattening drops the tombstone along with the zone"

"No addressable-but-unwritten state exists in the API, so there is no zeroing pass and no stale-data window: a fork or fresh VM cannot observe bytes nobody wrote. This is the dm-thin zeroing problem from lesson 1 deleted rather than mitigated."

Quizzes (inferred correct): snapshot of 1 TB takes "Microseconds: freeze the root record under a name"; a 10,000-deep lineage dying costs "At most ~32" decrements; a reset inherited zone reads as "Zeros, because the child's delta records a tombstone."

### Numbers, invariants, failure modes

| item | value |
|---|---|
| snapshot cost | microseconds, zero bytes |
| fork cost | 1 refcount increment + 1 root write, sub-ms group commit |
| skip node interval | ~32 levels |
| max chain depth required | 10,000 |
| max hops per read / delete cascade | ~32 |
| full zone table, 128 TiB disk | ~24 MiB (16 B/zone + 32 B/segment) |
| flatten rate | ~10/s fleet-wide |
| segment cut at fork in example | 92 MiB |

Invariants: increment before the reference exists; a skip node has no parent ref; tombstones mask inherited zones; roots stay kilobytes; flatten copies names only.

Failure mode: crash mid-flatten leaks (safe direction) and the auditor heals it.

### Lesson for our design

segstore makes a root "a pointer plus a delta" rather than a flat map because a flat map makes every fork an O(size) refcount walk, and it caps chain depth with content-addressed skip tables because otherwise read cost scales with ancestry. For our design: the "small offset→hash map" we plan to move on migration *is* a flat root. It is fine for a research system with no fork chains, but if the study includes snapshots or forks (it should, for the capacity study), three things transfer directly:

1. A fork should be a parent pointer plus an empty delta, not a copy of the map, or fork cost is O(disk). Store the flattened map itself as a chunk so identical maps dedup and roots stay small.
2. Discards on a forked disk need tombstones in the child delta, or inherited data reappears after a guest delete.
3. Take references before committing the new root, drop the old ones after. A crash then leaks, which the mark-and-sweep GC repairs; the reverse order can free live chunks.

The "no addressable-but-unwritten state" point is a free correctness property for us as well: an offset with no map entry reads as zeros, and there is nothing to zero on allocation.

---

## 20. Migration and node death

Lede: "An 800 GB VM moves to another node. How much of its disk crosses the wire, and why is node death the same procedure minus the RAM?"

### Design and reasoning

"Compute NVMe is a private cache and staging area, never pooled into a fabric. Nothing on it is the only durable copy (law 3), so node death and live migration are one procedure: the root record moves, the undestaged working set replays from the journal on the new node, and destaged data does not move."

Three steps:

1. **Precopy.** "The VMM streams RAM while the guest runs (stock QEMU or cloud-hypervisor). Disk needs no dirty tracking: sealed is immutable and remote, and every open-tail byte was journaled at ack."
2. **Pause, tens of milliseconds.** "Drain in-flight I/O. Virtio queue state and the zone table ride the migration stream. The destination daemon attaches, replays the journal from the destaged pointer D up to the durable watermark E, and the root swings."
3. **Resume.** "The cache warms via read-group prefetch. The first seconds read slightly cold, and that is the entire lasting cost."

"Cooling segments the destination lacks are fetched during precopy, so the pause sees only the open tails. Local-class volumes journal their tails during precopy, or drain. Ephemeral five-minute forks never migrate: respawn is cheaper."

Quizzes (inferred correct): an 800 GB disk migration moves "Only what has not destaged yet: open tails plus any cooling segments, replayed from the journal." When node A burns, its data is "Replayed from the storage server journal onto a new node."

### Numbers, invariants, failure modes

| item | value |
|---|---|
| pause | tens of milliseconds |
| disk bytes moved | open tails + cooling segments the destination lacks |
| ephemeral fork lifetime | ~5 minutes; never migrated |

Invariants: law 3 (compute NVMe is never the only durable copy); replay window is exactly (D, E]; no dirty-block tracking on disk.

Failure mode: local-class volumes have no remote journal, so they must journal their tails during precopy or drain, or they are lost on node death.

### Lesson for our design

segstore gets "migration = node death + RAM" because every acked fleet-class byte already sits on a remote journal before the ack returns. **Our design deliberately does not do this: the write is acked after local fdatasync with no network on the path.** The consequence is the exact asymmetry this lesson names:

- Migration works as planned. The staging log is drained (or shipped) during precopy, the offset-to-hash map rides the stream, and the destination pulls chunks by hash on demand. Only the undrained log tail and the map cross the wire. The "read slightly cold" cost is our cold-remote-read study item (3), and read-group prefetch is segstore's mitigation for it.
- Node death is not the same procedure. The staging log on the dead host is the only copy of everything acked since the compactor last ran. segstore's term for a volume with that contract is *local class*: "dies with the host by contract." Our design as stated is local-class for the write path and fleet-class (k replicas) only after compaction.

For the paper this must be stated as the durability contract: RPO equals the compaction interval, and the study should measure it. The honest framing from lesson 26 is that "fleet durability faster than a network round trip" is a battery, and our system chooses local ack instead. If the study wants node-death survival, the fix is the segstore one: replicate the staging log tail to the peer before ack (which puts the network back on the write path), or shorten compaction to bound the window.

---

## 21. Placement, repair and oversight

Lede: "Where a shard lives is written down once. When its drive dies the fleet rebuilds it without anyone noticing, and when OVH powers a whole box off for the swap, nothing is lost either."

### Design and reasoning

Placement flow at seal: "split into 11 shards of 32 MiB" → "pick eleven drives: emptiest and least busy of the allowed set; power of two choices" → "allowed set: anti-affinity. eleven distinct drives, at most three shards per box (one each once there are eleven boxes), racks and power feeds next" → "reserve, then stream: the target daemon reserves the fixed 32 MiB first; refused? re-pick" → "record in the index: location is written down, never recomputed".

Hash placement (CRUSH) versus recorded placement:

| hash placement | recorded placement (segstore) |
|---|---|
| no index to keep: the name says where the shard lives | an index exists anyway, for refcounts and liveness |
| adding a drive moves a share of every placement: rebalance storms | adding a drive changes future choices only |
| no way to route around a full or busy drive | a full or slow drive is simply not chosen |
| a failed reservation has nowhere else to go | reservation at admission closes the choose-versus-fill race |

"Tenants contend for aggregate quota, never for devices. The storage daemon owns its drives, keeps RAM free-counters, and says no before bytes flow."

Losing a drive, the loop that runs without a human:

1. **detect:** "a read error inside the one-second recovery limit, a SCSI sense code, a grown-defect entry in the drive's log page, a scrub that reads a shard and gets the wrong hash, or a daemon heartbeat that stops. Any of these marks the drive suspect; two in a row marks it failed."
2. **fence:** "the index flags the drive failed. Placement stops choosing it that instant; reads route around it; nothing waits."
3. **enumerate:** "the index answers drive → shards from RAM: which segment, which of the eleven. A 24 TB drive holds about 700,000 of them."
4. **rebuild:** "for each shard, read any 8 siblings, recompute the missing one, land it on a drive the anti-affinity rule allows, verify against the segment hash, record the new location."
5. **forget:** "the old location is dropped. If the drive ever comes back its superblock generation is stale and its slots are ignored."

"Budget it, do not hope: a full 24 TB drive is 24 TB × 8 sibling reads ≈ 190 TB of reads. At 2 GB/s of fleet repair read budget that is about a day, and the day is spread over every other drive in the fleet, not concentrated on one. During that day one more failure in the same segment costs nothing and two cost a reconstruction from exactly 8."

Losing a whole box on purpose: "OVH replaces a drive in a dedicated server by powering the server off. So a dead drive turns into a planned outage of 36 drives and 864 TB, and the design has to make that boring."

- "why it holds: at most three shards of any segment on one box, and three parity shards. so with one box dark every segment still has at least 8 readable shards, which is exactly enough. the anti-affinity rule in the Flow above is not about balance. it is this."
- "maintenance, not failed: the operator marks the box maintenance-until-T before the ticket. placement excludes it, reads reconstruct around it, and repair does NOT start rebuilding 864 TB. if the box is not back by T it flips to failed and the loop above runs on all 36 drives"
- "journal groups first: each group with a member on the box gets a replacement member before shutdown. tails are small, so this is seconds; the box's copies are dropped. a group at 2 of 3 still has quorum, but a second failure during the window would not, so we do not run the window at 2"
- "coming back: drives re-register, slots re-scan. shards are immutable, so nothing on the box is stale; slots whose segment died while it was dark are reclaimed by the normal delete path. no resync, because nothing was ever written in place"

Operator signals: per drive (defects, pending sectors, temperature, recovery-limit hits, read error rate, scrub position and mismatch count, free slots; "two suspect signals in a row is the auto-fence"); per segment ("how many of 11 shards are readable, as a histogram. 11 is healthy, 10 is repairing, 8 is critical. the one chart to watch. any segment at 8 pages someone"); repair queue ("time to drain longer than the deadline of any maintenance window is the alert"); journal groups ("a group at 2 of 3 outside a maintenance window is a failure in progress"); per box ("live, maintenance until T, or failed with the ticket number next to T. the only state a human sets by hand").

"Two things stay human: opening the OVH ticket, and choosing depop over replacement for a drive with one dead head."

### Numbers, invariants, failure modes

| item | value |
|---|---|
| shards per box | at most 3 (1 each at 11+ boxes) |
| drive size | 24 TB, ~700,000 shards |
| repair reads per drive | 24 TB × 8 ≈ 190 TB |
| repair budget | 2 GB/s fleet-wide, ~1 day per drive |
| box | 36 drives, 864 TB |
| detect to fail | 2 suspect signals in a row |
| shard health histogram | 11 healthy, 10 repairing, 8 critical |
| journal quorum | 3 members, ack at 2, never run a window at 2 |

Invariants: location is recorded, never recomputed; reserve before stream; anti-affinity is sized so one dark box leaves exactly k readable shards; immutable shards mean returning boxes need no resync; a returning drive with stale superblock generation is ignored.

### Lesson for our design

segstore uses recorded placement with a "power of two choices" pick because an index exists anyway and hash placement cannot route around a full or slow drive or handle a refused reservation. Our design uses rendezvous hash. For two hosts, rendezvous hash with k=2 degenerates to "both hosts hold everything" and with k=1 to "each host holds half"; there is no third choice to route to, so the difference between recorded and hashed placement mostly vanishes. What does transfer:

- **Reservation before stream.** The receiving host should admit a chunk (or container) by reserving space and refusing when full, so the sender fails cleanly rather than overfilling. With two hosts a refusal means "store locally and mark under-replicated," which needs a repair queue.
- **Immutable units make return-from-outage free.** If host B goes down and comes back, nothing on it is stale; only chunks that died while it was dark need reclaiming. The mark-and-sweep GC with epochs already covers this if the epoch check is done against B's local set on rejoin.
- **Repair is enumerated from the index, not by scanning.** Keep a per-host "which chunks does the peer hold" record so that repair after a host loss is a list, not a walk.
- **The health signal to expose is the per-chunk replica-count histogram** (k, k-1, 0) and the repair-queue drain time.

Anti-affinity and box-level maintenance do not apply beyond "the two hosts are the two failure domains." Lesson 27 has more on why a two-box fleet is the worst case.

---

## 22. The recoverable index

Lede: "The map of every shard in the fleet fits in RAM. Losing all of it is an inconvenience, and rebooting the whole fleet is four lookups."

### Design and reasoning

RAM tables: "hash → shards, refcounts, quotas: ~4M segments/PB × ~128 B ≈ half a gigabyte per petabyte". Durability: "metadata journal: every mutation appends here first" → "periodic checkpoint: the tables serialised whole" → "root ring advances: the next slot names the newest checkpoint; a torn write fails its checksum and the previous slot stands".

"Recovery is: load the newest checkpoint, replay the journal tail. Lose all of it and the index rebuilds by listing the drives, because every slot names its own contents. Hours on a 20 TB disk, and not the plan, but it is the property that separates this from an FTL whose mapping loss is data loss."

"'The index' is really five maps with different owners. Ownership decides who can answer a question without asking anyone, and what a crash costs."

- **root records:** "per VM: parent pointer + the zones it wrote itself · control plane, sharded by family tree. the one irreplaceable thing: segments do not know their owners"
- **segment refcounts:** "segment → live reference count · one home server per segment: the holder of its first data shard. a zone reset from a compute node is an unpin RPC to the home, idempotent per owner" (revised in lesson 27)
- **shard placement:** "segment hash → drives and slots · owned by the server holding the drives. plus a fleet record of which servers hold a given segment"
- **blob index:** "blob hash → segment, offset, length · segment-level entries in RAM; blob-level entries in per-segment manifests. a billion 200-byte blobs would rival the data; manifests are paged in like a packfile index"
- **arena map:** "what is in this node's NVMe right now · compute node RAM only. authoritative for nothing; rebuilt on restart"

Cold start: "Power-cycle everything at once, so every RAM table in the fleet dies together, and then start a VM."

- "control plane quorums: fixed, well-known addresses; checkpoint + log replay from their own SSDs. the only fixed addresses in the fleet. what breaks the who-stores-the-storer circle"
- "storage daemons rebuild: checkpoint + metadata journal; worst case, relist the drives"
- "compute nodes register: receive assignments, open pools; their NVMe is a cache nobody needs"
- "VM start: root record → segment names → placement → journal entries in (D, E]. four lookups, zero scans. destaged pointer D and durable watermark E bound the replay exactly"

Seed file: "three to five pinned intranet IPs, present on every machine's local disk as part of the node image. The only storage a machine has before it can talk to anyone is its own disk, so the recursion has to bottom out there, not in DNS, DHCP, or cloud metadata... Ceph's monitor map and etcd's initial-cluster are this same design."

On-disk: "Raw drives, JBOD, no RAID under EC. Each HDD is a superblock plus a grid of 32 MiB slots with a free-slot bitmap; shards stream into slots; reclaim clears bits; a tiny root ring per drive (a few checksummed, sequence-numbered slots) points at the newest index checkpoint. It is the same append-plus-root design applied one level down. A filesystem here would pay its journal twice over for guarantees that immutable, content-named, fixed-size shards already have. The TCP protocol (journal.append, shard.put, get, pin, have, scrub, all idempotent by name) makes the layout swappable."

Quizzes (inferred correct): RAM index is safe because "durability comes from the journal plus checkpoint; RAM is only the serving copy"; starting one VM after a fleet power cycle scans "Nothing... Four lookups."

### Numbers, invariants, failure modes

| item | value |
|---|---|
| index RAM | ~4M segments/PB × ~128 B ≈ 0.5 GB per PB |
| full rebuild from drives | hours per 20 TB disk |
| seed file | 3 to 5 pinned IPs |
| VM start after cold start | 4 lookups, 0 scans |
| replay window | (D, E] |

Invariants: every slot names its own contents (self-describing on-disk format); root records are the one irreplaceable map; the arena map is authoritative for nothing; every wire op is idempotent by name.

Failure mode: torn checkpoint write is caught by checksum and the previous root ring slot stands.

### Lesson for our design

segstore keeps the index in RAM with journal plus checkpoint and makes the on-disk layout self-describing because the alternative, an FTL-style mapping whose loss is data loss, is the failure it was built to avoid. For our design, three direct consequences:

1. **The offset-to-hash map is the root record: the one irreplaceable thing.** Chunks do not know their owners. It must be journaled (the staging log can carry map mutations) and checkpointed, with a checksummed root pointer. Losing it loses the VM even if every chunk survives.
2. **The chunk store must be rebuildable by listing it.** Every stored chunk or container carries its own hash and length in a header, so the hash-to-location index can be rebuilt by scan as the disaster path. This is what makes the RAM index safe to lose.
3. **The per-host RAM cache is the arena map: authoritative for nothing, rebuilt on restart.** Never let GC or placement depend on it.

The blob-index point about "a billion 200-byte blobs would rival the data" is the per-chunk index size warning again: at 4 KiB chunks, a 1 TB disk is 256M entries; segstore keeps segment-level entries in RAM and pages blob-level manifests in. Consider the same split: container-level entries in RAM, chunk-level manifests per container.

---

## 23. Garbage collection

Lede: "The pool is filling. How does the collector know what is garbage, and which of its three mechanisms actually copies bytes?"

### Design and reasoning

Three mechanisms:

- **reclaim:** "a refcount hits zero. zone reset · unpin · root deleted · free the NVMe slot, unlink the eleven shards. O(1), immediate; most space comes back here"
- **compaction:** "a cooling segment is mostly dead. lowest live fraction first; on lanes where the host knows liveness (conventional disk, /cas) · copy the survivors, free the whole segment. no guest involved: the guest addresses zones, and the root table maps zones to segments"
- **auditor:** "background, rate-limited · mark and sweep from every root and pin. heals refcount drift; nothing ever waits on it"

Selection rule: "Clear the emptiest segment first and the copy bill stays near zero; start with the 96% one and you pay 246 MiB to reclaim 256. Same freed bytes, twenty times the work. Nothing here ever touches an HDD."

Indirection: "The guest addresses zones. The root table maps each zone to a segment and an offset range, and the guest never sees a segment name. So the host can copy a zone's bytes into a different segment, update the table, and the guest keeps reading zone 7 as if nothing happened. What the host cannot do on the zoned lane is drop dead blocks inside a zone, because only XFS knows which blocks in a finished zone are still referenced."

How a delete reaches the host (zoned lane): guest deletes a file, XFS marks blocks free in its own metadata; "Nothing reaches the host. Zone 7 looks unchanged from below"; XFS zone GC evacuates zone 7; "XFS resets zone 7. The reset is the delete. The host drops the segment's reference; at zero the eleven shards unlink and 352 MiB comes back. If a snapshot still names the segment, it stays." "On the boot disk the host owns the block map, so a discard for an LBA removes its entry directly and the host can repack. On /cas, unpin is the delete. Only the zoned lane needs the guest to finish the job."

Where drift comes from: "On one machine it cannot: a refcount change and the root write that causes it are the same journal transaction. Drift lives in the places that transaction cannot cover."

- "two machines: destage writes eleven shards on the storage tier → the index records placement and takes the reference → crash in between: shards no entry names. a leak, which is the safe direction; scrub finds orphan slots"
- "retries: every wire operation is idempotent by name, so a retried put is safe · a retried pin must be idempotent per owner: a set, not a counter. or the second attempt counts the same reference twice"
- "chain flattening: a skip node takes direct refs on every segment it names → then drops its parent ref → an interrupted move is where counts go wrong. ~10/s fleet-wide: the largest source by volume"
- "shard cuts: a cut edge is a reference across two consensus groups · the child side holds a pin so a sweep in the parent shard treats the cut as a root. rare, and covered"
- "bugs: refcounts are derived state; reachability from roots is ground truth. the honest one"

"Increment before the reference exists. Decrement after it is gone. A too-high count leaks space, and the auditor sweeps it up. A too-low count frees data someone can still reach, which no sweep can undo. The asymmetry is why the auditor is allowed to be slow."

Scale: "Bookkeeping is segment-granular: about 4 million refcounts per petabyte, in RAM. A segment at zero references frees by unlink, all eleven shards deleted whole, on any tier. Survivors of a mostly-dead segment are copied by the lane's copier (XFS on the zoned lane, the host repacker on the others; the slow-leak lesson), lowest live fraction first. Death-group hints and fork lifecycles make segments die together. The auditor's sweep walks metadata, about 0.01% of the pool, not data. Urgency comes from the pressure ladder: at ~70% full, the auditor and compaction speed up and guests are asked to volunteer their emptiest zones."

Quizzes (inferred correct): the collector finds garbage because "It already knows: every delete, unpin, and root removal decremented a counter"; bytes referenced by yesterday's snapshot are "No: the bytes stay until every root referencing them is gone"; the survivable wrong direction is "Too high."

### Numbers, invariants, failure modes

| item | value |
|---|---|
| compaction worst case | copy 246 MiB to reclaim 256 (96% live) |
| segment on HDD | 352 MiB (256 × 11/8) |
| refcounts | ~4M per PB |
| auditor sweep | walks ~0.01% of pool (metadata only) |
| flatten rate | ~10/s fleet-wide, largest drift source |

Invariants: increment before the reference exists, decrement after it is gone; pin is a set per owner, not a counter; every wire op idempotent by name; refcounts are derived, reachability from roots is ground truth; too-high is safe, too-low is fatal; nothing waits on the auditor.

### Lesson for our design

segstore runs three collectors, not one, because each does a job the others cannot: refcounts free most space in O(1) at the moment of the delete; compaction is the only thing that copies bytes and picks emptiest-first; mark-and-sweep is ground truth for drift and is allowed to be slow because it only ever fixes the safe (too-high) direction. **Our design as stated has only the third.** Mark-and-sweep with epochs as the sole collector means:

- Space does not return when a VM is deleted or a block is overwritten; it returns at the next sweep. On a two-host system with a few TB this may be acceptable, but the study's capacity numbers will show garbage between sweeps, so report "reachable bytes" separately from "stored bytes."
- There is no compaction, so containers (if we pack chunks) decay toward one live chunk. Lesson 24 covers this.
- Sweep walks metadata only if the offset-to-hash maps are the roots and chunks are leaves. Keep it that way: never require reading chunk bodies to mark.

What to add cheaply: per-chunk (or per-container) refcounts as derived state, incremented before the map entry that references them is committed and decremented after it is removed; keep the sweep as the auditor. Cross-host drift lives exactly where segstore says: a chunk pushed to the peer before the local index records it. Make that ordering leak-safe (push, then record, so a crash leaves an orphan the sweep finds) and make pushes idempotent by hash.

---

## 24. The slow leak

Lede: "A 4 MiB database rewrites itself all day. Guess what it costs on disk after a week, then meet the collector that never wakes up."

### Design and reasoning

Opening quiz (inferred correct): a 4 MiB SQLite database rewriting pages all day occupies "Unbounded: every destaged segment that still holds one not-yet-rewritten page costs 352 MiB, and nothing in the previous lesson copies on HDD."

Widget: "Each tile is 4 MiB (a thousand 4 KiB blocks) of one 256 MiB segment that already destaged to HDD as eleven 32 MiB shards. Rewriting the blocks in a tile kills it here and puts fresh copies in some newer segment. Reclaim on HDD is unlink-only, so the 352 MiB stays until the last tile dies or a collector copies the survivors out and resets the zone." The zoned-XFS branch: "disk is thin-provisioned at 128 TiB. XFS sees 524,000 free zones. its collector never wakes up."

Who can see the garbage:

- "zoned XFS disk: the host sees a zone as written or reset, nothing finer · only XFS knows which blocks in the zone are live. the host cannot compact this lane"
- "conventional disk: the host owns the LBA map · it knows exactly which extents are current. the host can copy survivors"
- "/cas blobs: the host owns blob refcounts · it knows which blobs in a segment are still referenced. the host can copy survivors"

"Whoever can see liveness inside a segment is the one who copies. Nobody copies twice. On the zoned lane that is XFS zone GC in the guest. On the conventional disk and on /cas it is a host repacker. The host is always the one who unlinks."

The thin-disk trap: XFS zone GC "runs when free zones get scarce. Our data disk advertises 524,288 zones and a VM writes into a few hundred of them." As shipped: "zone GC triggers on free-zone count; a thin 128 TiB disk never runs low on free zones; so the collector never wakes, and no zone is ever reset by GC; every rewritten page leaks its old copy on the host, forever." Patched: "zone GC also triggers on garbage ratio: a zone under ~25% live is evacuated when the guest is idle; XFS reports per-zone live counts over the hint lane; a host pressure event names the emptiest zones and asks for them by number; the reset lands as an ordinary decrement; the host unlinks 352 MiB." "This is the load-bearing guest patch. Without it the zoned lane has no working collector at all on a thin disk, and the flash and HDD bills for long-lived VMs grow without bound."

The host repacker: "pick: destaged segments below a live-fraction threshold, emptiest first" → "read the survivors: range reads from the data shards; one seek per extent" → "append into a fresh segment: on the storage tier, no compute node involved" → "seal, destage, record forwarding: old name → extents in the new segment" → "unlink the old shards: 352 MiB back". "Rate-limited and background, like the auditor. It reads only live bytes, so a segment at 2% live costs 5 MiB of reads to free 352 MiB of disk."

Forwarding: "A copier gives the survivors a new content name. Referrers are never rewritten; the placement index maps the old name to extents inside the new segment. Otherwise every snapshot, fork and skip table that names the old segment would need updating, across consensus shards, on every compaction. Forwarding entries are rewritten in place when their target is repacked again, so a name is never more than one hop from bytes, and a forwarded name keeps its refcount home: the home follows the entry, not the vanished shards."

What is patched: "Host and storage tier: zero kernel changes. vhost-user, virtiofsd and TCP are all userspace, with io_uring on raw NVMe." Already mainline: "virtio-blk zoned (6.3) · zone append (5.8) · zoned XFS (~6.15) · virtio-fs + DAX, ext4". Guest patches: 1 zone GC trigger (load-bearing), 2 hints ("per-write death and read groups · so segments die together and prefetch has a unit"), 3 events ("event virtqueue surfaced as uevents"), 4 /cas glue (vsock put/pin/have, io_uring passthrough). "An unpatched guest still boots and runs. It is mute about hints and deaf to pressure, and on a thin disk it leaks."

Is 256 MiB too big? "what the size buys: 32 MiB shards keep HDD seek overhead near 5% · about 4 million index entries per petabyte." "what it costs: a segment dies whole less often · one live page pins 352 MiB until a collector runs. granularity, not the leak itself." "what fixes the leak: a collector on every lane, with a trigger that fires on a thin disk · forwarding so copying is cheap. halving the size would halve the pinned bytes and fix nothing." "Bench 128 MiB against 256 MiB at bring-up as a hedge. The constant is a tuning knob; the collectors are the design."

Quiz (inferred correct): snapshots naming the old segment after compaction: "Nothing. The index records that 9f3a… now lives at an offset inside c41e…, and reads follow one extra hop."

### Numbers, invariants, failure modes

| item | value |
|---|---|
| tile in widget | 4 MiB = 1000 × 4 KiB |
| HDD bill per segment | 352 MiB, 1.4× (1.375) per live byte when full |
| thin disk zones | 524,288 advertised, a few hundred used |
| garbage-ratio trigger | under ~25% live, when guest idle |
| repack cost at 2% live | 5 MiB read to free 352 MiB |
| forwarding depth | never more than one hop |
| seek overhead at 32 MiB shards | ~5% |
| index entries | ~4M per PB |
| kernel versions | zone append 5.8, virtio-blk zoned 6.3, zoned XFS ~6.15 |
| hedge bench | 128 MiB vs 256 MiB segments |

Invariants: whoever sees liveness copies; nobody copies twice; the host always unlinks; referrers are never rewritten (forwarding instead); a forwarded name keeps its refcount home.

Failure mode: a collector whose trigger depends on free-space count never fires on a thin disk, so every rewrite leaks its old copy forever.

### Lesson for our design

segstore's key claim here is that a log-structured, unlink-only store leaks unboundedly under in-place rewrites unless a copier exists on every lane with a trigger that fires on a thin disk, and that the unit size is a knob rather than the fix. For our design this is the most direct warning in the whole range:

- **Our daemon owns the offset-to-hash map, so it is the "conventional disk" lane: the host can see liveness and must be the copier.** No guest patch needed, which is a real advantage over the zoned lane. But the copier has to exist. If chunks are stored individually, "compaction" is just deleting unreferenced chunks (no copying), and the leak is the per-chunk index cost instead. If chunks are packed into containers, we need a repacker with emptiest-first selection and a forwarding entry so the map is not rewritten.
- **Trigger on garbage ratio, not free space.** A research testbed with a mostly-empty disk will never hit a free-space threshold, and the SQLite-rewrite workload in this lesson is the exact test that exposes it. Include it in the study: a small database rewriting pages for hours, and plot stored bytes versus reachable bytes.
- **Guest discard is the delete signal for a block device.** Without discard (or with a guest that does not issue it), the daemon sees overwrites only; deleted-but-not-overwritten blocks stay reachable forever. Enable discard in the guest and treat it as a map tombstone.
- **The "4 MiB SQLite database costs unbounded HDD" quiz is a benchmark to run against ZFS dedup** in study item (1): ZFS with dedup frees on overwrite because its DDT refcount drops, so it has no equivalent leak. Parity means our design must not lose that.

The kernel-version list (zone append 5.8, virtio-blk zoned 6.3, zoned XFS ~6.15) is irrelevant to us; we run a conventional block device on stock QEMU.

---

## 25. Building S3 on /cas

Lede: "An object store is an HTTP dialect plus an index. If the gateway needs one new host primitive, the design failed."

### Design and reasoning

"An interface is proven the way an instruction set is: by what you can build on it without asking for new instructions." Gateway VMs terminate S3. "Object bytes and the bucket index are different kinds of data, so they take different lanes."

PUT: client HTTP into gateway → "bytes → /cas put: a content name comes back. big, immutable. EC, scrub, tiering: already handled below" → "pin the name: under the tenant's pin set: this is what keeps it reachable" → "index entry: (bucket, key) → name. small, mutable, ordered. the database part, on the gateway's own disk" → "200 OK after a fleet-class fsync of the index".

GET: "index lookup → read the blob by name. self-verifying: the name is the hash. no gateway state touched but the index". Multipart: "each part is a blob → complete writes one manifest listing the parts. assembly copies zero bytes". DELETE: "drop the index entry → unpin the name → the decrement does the rest". Versioning: "keep the old names in the index · unchanged bytes dedupe themselves. same content, same name. versioning is an index feature". Lifecycle to cold: "sealed segments are already S3-shaped · the tier destages; the gateway does nothing. no chunking bridge to build".

"Count what the gateway implements: an HTTP dialect and an index. Durability, placement, repair, scrub, erasure coding, and tiering all sit below it, already built. That is why this is a small program and not a storage system."

Who owns the index: "every writable disk has exactly one live VM writer. The root record enforces it, because a root names its owner and ownership changes only by the atomic swap. So the bucket index is owned by one VM per shard, full stop. This is not a limitation to engineer around: block devices shared writable between hosts need cache coherence across machines, which is a different and much worse product."

Scale-out: "writes: shard the bucket namespace across gateways · each shard's index disk has its one owner. single writer per root; many roots"; "reads: any gateway serves any object · RPC to the shard owner returns the blob NAME; the asking gateway reads the blob itself. pass names, not bytes. pins are tenant-wide, so every gateway in the tenant can reach what any of them pinned"; "growth: splitting a shard = forking the index VM → each half drops the other's keys at leisure. fork copies zero bytes".

"RPC between gateways carries keys and names. Object bytes never proxy through the index owner: whichever VM holds the client socket reads the blob from /cas directly, and the security lesson's reachability gate is what says it may."

Quizzes (inferred correct): PUT bytes go "Into the /cas lane, keeping only the returned content name in the index"; a cross-gateway GET moves "The key over RPC, and the blob name back; gateway 7 then reads the blob from /cas itself."

### Numbers, invariants, failure modes

No numeric constants. Invariants: exactly one live writer per writable disk, enforced by the root record's owner field and changed only by atomic swap; object bytes never proxy through an index owner; pins are tenant-wide; versioning and multipart are index features that copy zero bytes.

### Lesson for our design

segstore proves its interface by building S3 on it with no new host primitive, and the single-writer-per-root law is what makes the scale-out story mechanical. For our design: **the single-writer invariant is one we already hold implicitly (one VM, one vhost-user-blk device, one daemon) and should state explicitly, because migration is where it breaks.** During migration the offset-to-hash map must have exactly one owner at every instant; the swap of ownership is the atomic step, and the source daemon must refuse writes after it. The "pass names, not bytes" pattern is our cross-host protocol as well: the peer returns chunk hashes or chunk bodies, never proxies a VM's I/O.

The S3-on-CAS application itself does not apply, but it suggests an easy demonstration for the study: a snapshot export is the set of chunk hashes reachable from a root, and a backup to object storage is "upload chunks under their own names, never re-upload."

---

## 26. An LSM store on zones

Lede: "Databases that write like this system wants everyone to write pay garbage collection once. Put them on the wrong disk and they pay twice."

### Design and reasoning

"LSM databases already write the way this system wants everyone to write: SSTables are immutable, written in full, and deleted whole. That is the segment contract, verbatim, which makes the mapping nearly mechanical."

- "memtable flush: open a zone, append the SSTable, finish it · one sequential stream exactly what the zone asked for. an SSTable is a segment"
- "write-ahead log: small plain volume, fleet class · commits in ~0.5 ms via the journal or local class at ~40 µs if the operator accepts host-loss risk. durability class chosen per volume"
- "compaction: read N SSTables → write the merged one → reset the N zones. a reset is a decrement, not an erase"
- "block cache, temp: ephemeral volume: local class, no destage · dies with the host, on purpose. declared disposable, so nothing is journaled or destaged"

"The payoff is one collector instead of two. On the conventional disk this database pays garbage collection twice: its own compaction rewrites live values, and beneath it the host repacker rewrites the same bytes again to reclaim half-dead segments it cannot see into. On the zoned device deleting an SSTable is deleting the segment. Every byte is copied once, by the layer that understands it."

"Tally what got used: durability classes (WAL fleet, caches local), the /cas lane (blobs by name, dedup, self-verification), single-writer roots (index ownership), forks (read replicas, shard splits), zones (the LSM alignment). Neither service asked the host for anything new."

"The falsifier: a workload that genuinely needs multi-writer shared block storage, or fleet durability faster than a network round trip. This design says no to both, on purpose. The first ask is a distributed cache-coherence protocol wearing a disk costume; the second is a battery."

Quiz (inferred correct): "The conventional run pays write amplification twice: compaction above the device, LBA-map GC below it."

### Numbers, invariants, failure modes

| item | value |
|---|---|
| fleet-class fsync | ~0.5 ms via journal |
| local-class fsync | ~40 µs |

Invariant: every byte is copied once, by the layer that understands its liveness.

Failure mode named as the design's falsifier: multi-writer shared block, or durability faster than a network RTT.

### Lesson for our design

Two numbers and one framing transfer. The numbers: segstore's own local-class ack is ~40 µs and fleet-class is ~0.5 ms; **our "ack after local fdatasync" is local class, and the study's write-latency baseline should land near the 40 µs figure on NVMe, with the ~0.5 ms figure as the cost of the network-on-write-path alternative we chose not to build.** The framing: "fleet durability faster than a network round trip... is a battery." Our design takes the local ack and pays for it with the node-death RPO from lesson 20; the paper should say this in one sentence rather than let a reviewer find it.

The double-GC point applies to our compactor in a weaker form: a guest filesystem or database on top of our block device already does its own compaction, and our chunk-level dedup then re-chunks the result. Content-defined chunking is what keeps that from being a second full copy, since shifted-but-identical data re-dedups. Measure write amplification (guest bytes written versus bytes landed in the chunk store plus replicas) as a study metric.

---

## 27. Where it breaks

Lede: "Six places the design is thinner than it looks at a hundred thousand VMs, what each one costs, and the change that closes it. Two of them change decisions made earlier in this course."

### Design and reasoning

Opening quiz, "Start with the one that is true today": with two storage boxes and one powered off, fleet-class reads of cold data see (inferred correct) "Cold reads of every segment with 6 shards on the dark box stall until it returns; nothing is lost."

**The code depends on the box count.** "Anti-affinity was written as a fixed rule, at most three shards per box. It is really a function of how many boxes there are, and so is the code."

| boxes | code | overhead | note |
|---|---|---|---|
| 2 | 4 + 4, split 4 and 4 | 2× | "one box dark leaves 4, which is exactly k. this is the price of two giant boxes; the alternative is the stall above" |
| 3 | 8 + 4, split 4, 4, 4 | 1.5× | one box dark leaves 8 |
| 4 to 10 | 8 + 3, at most 3 per box | 1.375× | "four is the minimum fleet for the design as written: 3 journal members on distinct boxes plus one that can be dark" |
| 11 or more | 8 + 3, one per box | 1.375× | "a box outage costs each segment at most one shard. and now a second failure during the outage is a single-shard event, not a critical one" |

"So the code is a per-segment attribute chosen at seal from the current box count and recorded in the index next to the placement. Adding boxes changes only future seals; a background repacker re-encodes old segments to the cheaper code when it has nothing better to do. Buy more, smaller storage boxes rather than fewer huge ones: the maintenance domain at OVH is the box, and 864 TB is too much box for a two-box fleet."

**Refcounts must not live on one box.** "An earlier lesson put a segment's refcount home on the box holding its first data shard. During a box outage every fork and delete that touches a segment homed there would wait, up to the maintenance deadline. A fork is a refcount increment before anything else, so the cheapest operation in the system would be the one that blocks." Was: "refcount on the first-shard box in that box's metadata journal. local to one shard, remote to the other ten anyway." Now: "refcounts in the replicated metadata groups. the same sharded consensus that holds roots; 8 bytes per segment, so the whole table is small. a dark box never blocks a fork or a delete. GC already had to message ten boxes to free a segment; now it messages eleven."

**The journal has a pressure ladder too.** "The journal SSD is sized from an assumption: about a tenth of written bytes are fleet class. A fleet that all starts running fsync-heavy databases at once breaks the assumption, and the failure has to be loud and gradual, never a full SSD refusing appends."

1. "shorten cooling: segments seal and destage earlier, so entries retire sooner. Costs a little more HDD garbage later."
2. "destage ahead of the window: the oldest undestaged segments go now, out of order. Costs HDD bandwidth that was reserved for repair."
3. "slow the ack: group-commit windows stretch, so fsync latency rises for the group's VMs. Visible, proportional, and still durable."
4. "refuse fleet-class appends for the group: the last rung, reached only if the three above did not hold. Local class is unaffected throughout."

"Metric: hours of retention left per group at the current append rate. The ladder starts when it drops under two hours and each rung is a logged state change, the same shape as the capacity ladder."

**Cold reads are the scarce resource.**

- "the wall: 36 drives × ~150 random reads a second ≈ 5,400 per box. a single VM scanning cold data at 4 KiB can take all of it. bandwidth is plentiful; seeks are not"
- "the budget: a per-VM cold-read token bucket, enforced at the compute daemon. random 4 KiB misses spend it; whole-shard sequential reads are nearly free and do not. the noisy neighbour is throttled where it originates, before the wire"
- "why restarts are fine: a VM's recent data is a handful of segments, because the log wrote it that way. so a cold start prefetches whole shards sequentially: 125 GB of working set is 4,000 shards, minutes at drive speed, not a million seeks. log structure turns the IOPS problem into a bandwidth problem"
- "why misses are rare: 46 to 92 TB of NVMe per compute box for ~250 VMs. a destaged segment stays local until pressure evicts it; for most VMs that is never. the HDD tier is durability and depth, not the read path"

**Smaller things that are still true.**

- "ack quorum: 2 of 3 journal members, the third catches up. a member more than a bounded distance behind is replaced. otherwise the slowest SSD in the group sets every fsync"
- "index by scan: rebuilding one box's slot table from headers is 27 million reads, about 90 minutes at 5,400 a second. the checkpoint and metadata journal on the FDP pair are the boot path; the scan is the disaster path. checkpoint often; a lost checkpoint costs an hour and a half of dark box"
- "turning FDP on: a reformat of drives that are 96% full of production today. so the first compute box has to be emptied onto others before it can be formatted. an operations project that precedes the storage project"
- "Compute box loss. About 125 GB of undestaged fleet-class data per box, replayed from three journal servers in minutes. Local class dies with the box by contract, and compute NVMe has no RAID: the drive is the failure domain, and a local-class volume lives on one drive."
- "Repair amplification. Every rebuilt shard reads eight. Fine while repair is rare; at thousands of drives it becomes the largest background load. Locally repairable codes cut a single-shard rebuild to about half the reads. Later, when measured, not now."
- "The network we assume. Two bonded 25 Gb/s NICs. OVH guarantees private-network bandwidth per server plan, and it is often below line rate. Measure the vRack before trusting the 50."

Closing quiz (inferred correct): the durability risk, as opposed to availability or performance, is "Two more drive failures inside the same segment while a box is dark, at 8+3 with three shards per box."

### Numbers, invariants, failure modes

| item | value |
|---|---|
| 2-box code | 4+4, 2× overhead |
| 3-box code | 8+4, 1.5× |
| 4 to 10 boxes | 8+3, 3 per box, 1.375× |
| minimum fleet for design as written | 4 boxes |
| refcount table entry | 8 bytes per segment |
| fleet-class share assumed | ~10% of written bytes |
| journal ladder trigger | under 2 hours of retention left |
| cold-read wall | 36 × ~150 ≈ 5,400 random reads/s per box |
| cold-start prefetch | 125 GB = 4,000 shards, minutes |
| compute NVMe | 46 to 92 TB per box, ~250 VMs |
| journal ack | 2 of 3 |
| index scan | 27M reads, ~90 minutes per box |
| compute box loss | ~125 GB undestaged, replayed in minutes |
| NICs | 2 × 25 Gb/s bonded, "often below line rate" |
| LRC repair saving | about half the reads |

Failure modes: two-box fleet stalls cold reads during a box outage; refcount home on one box blocks forks during outage; journal SSD fills under fsync-heavy fleet; a single 4 KiB scanner saturates a box's IOPS; slowest SSD sets fsync latency without bounded-lag replacement; lost checkpoint costs 90 minutes of scan; vRack below line rate.

### Lesson for our design

This lesson is the one written for a fleet our size, and its first section is literally the two-box case. segstore says a two-box fleet forces a 2× code (4+4, mirror-equivalent) because one box dark must leave k shards, and that the honest alternative is "cold reads stall until it returns; nothing is lost." **For a two-host research system this settles the replication question: k=2 (full mirror) is the only configuration that survives a host outage without a read stall, and it costs 2×, which is the same overhead segstore concedes at two boxes.** k=1 with rendezvous placement halves capacity cost and is exactly the "stall, nothing lost" option. The study should run both and report the trade as segstore states it, rather than as a shortcoming.

Three more that transfer:

- **Refcounts (or GC epochs) must not have a home on one host.** If host B holds the authoritative "is chunk X referenced" state and B is down, every fork and delete on A blocks. Keep reference state on the host that owns the VM root, replicated with the map, not with the chunk.
- **Cold reads are IOPS-bound, and the cure is log structure plus whole-unit prefetch.** For study item (3), the per-transport cost of a remote cold read is a per-request latency, but the throughput question is seeks. A migrated VM's working set is a handful of recent containers if the compactor packs chunks in write order; fetch those whole and sequentially, and per-chunk RPC latency stops mattering. Measure both a 4 KiB random-miss cost and a whole-container prefetch cost per transport (TCP, RDMA). Add a per-VM cold-read token bucket only if a noisy-neighbour experiment is in scope.
- **Measure the network before trusting it.** "Measure the vRack before trusting the 50." Our transport study should report measured link bandwidth and RTT, not nominal.

The journal pressure ladder is the fleet-class analogue of our staging-log watermark: name the retention metric ("hours of log left at current append rate"), and step through shorten-compaction-interval, compact-ahead, slow-the-ack, refuse, in that order.

---

## 28. The end state

Lede: "Every operation a VM can ask for is a root commit. Everything else is arithmetic, so check the arithmetic."

### Design and reasoning

"A VM, a snapshot, a backup and a fork are the same kind of thing pointed at the same bytes, and every operation on them is one root commit. Segment bytes never change; the index and the counts change only by journaled steps."

- "fork is the primitive: a thousand sandboxes off one golden image · duplicated roots, base bytes stored once. everything else is a variation on it"
- "the same move, four names: migration moves a pointer, backup retains a root · restore points a VM at that root, revert assigns one. incremental backup is a have() set difference"
- "cold tiers need no bridge: sealed segments are already S3-shaped: immutable, written in full, content-named · upload under their own names, never re-upload, verify by name. nothing mutable means nothing to chunk or dirty-track"
- "integrity is structural: reads verify themselves, scrub walks hashes · the leak and oracle classes are gone, not mitigated. a consequence of naming by content"
- "one truth, three collectors: refcounts for liveness, mark-sweep from roots as ground truth · a copier per lane, the host always unlinks. no hidden firmware GC underneath any of it"

Does it hold at 100,000 VMs:

- "spawns + deaths: 333/s each, every one an O(1) root commit · sharded consensus groups, batched. a single group commits 10k+/s; not the bottleneck. fork copies zero bytes"
- "fsyncs: ~500k appends/s fleet-wide, ×3 replicas · hundreds of journal groups, group commit. each storage server ~15% of one SSD's flush budget"
- "segment seals: 170 GB/s ÷ 268 MB (256 MiB) ≈ 630 seals/s · × 11 shards ≈ 7k placement records/s into RAM tables. why metadata is RAM + journal, not a database"
- "compute NVMe: 340 MB/s written per node · ~1 TB of the 2 TiB for cooling ÷ 340 MB/s ≈ 49 minutes. 10× the 5-minute mean VM life: churn dies on flash. the rest is open segments and read cache"
- "HDD ingest: cooling survivors × 1.375 EC overhead · a few GB/s across the whole storage tier. sequential 32 MiB slot writes"
- "virtual bytes: 100k × 128 TiB ≈ 12.2 EiB advertised · roots store only written zones; forks share ancestor tables. chains flatten past depth ~32. thin is a metadata property"
- "journal retention: fleet-class writes × (cooling window + destage lag) × 3 replicas · low single-digit TB of SSD per storage server. per-VM extents so one slow VM cannot block trim. the journal SSD is sized for retention, not latency"
- "read misses: an HDD serves ~100 random reads/s · 100k VMs at even 1 miss/s each is 1,000 drives of IOPS. so compute read cache plus a warm SSD tier on the storage servers carry the working set. the one line that sizes the SSD tier; measure the miss rate before trusting it"
- "long-lived churn: segments on HDD decay toward one live page · a collector per lane: XFS zone GC (garbage-ratio trigger) on zoned, host repacker on conventional and /cas. forwarding in the index keeps copying cheap"

"When a number moves (bigger VMs, longer lives, fsync-heavier guests) the fix is more groups, more servers, or a bigger cooling budget. Counts change; formats and laws do not."

The strategy in one box: "One pool: append-only segments, content-named at seal, EC 8+3 on raw-extent HDDs, journal + roots replicated on storage-server SSDs. Guests (our kernel) speak three honest lanes: zoned XFS for files, /cas for blobs, io_uring passthrough for segment-native software — all views of one root record per VM. Every hard operation is a root swap; fleet-class bytes are journaled at ack; ephemerals run local-class and mostly die on NVMe in cooling; only what survives cooling is destaged. S3/Glacier attach as cold tiers with zero translation (sealed segments are already S3-shaped). CDC dedup is a background compaction flavor, adopted if measured duplicate fractions justify it. GC is one collector with the whole truth: refcounts, mark-sweep from roots, heat from reads, urgency from pressure — coordinated with guests through the event lane instead of hidden in firmware."

### Numbers, invariants, failure modes

| item | value |
|---|---|
| VMs | 100,000 |
| spawns and deaths | 333/s each |
| consensus group commit rate | 10k+/s |
| fsyncs | ~500k appends/s, ×3 replicas, ~15% of one SSD flush budget per server |
| seals | 170 GB/s ÷ 268 MB ≈ 630/s, ≈ 7k placement records/s |
| compute NVMe write rate | 340 MB/s per node |
| cooling capacity and window | ~1 TB of 2 TiB, ≈ 49 minutes |
| mean VM life | 5 minutes |
| EC overhead | 1.375 |
| advertised virtual bytes | 100k × 128 TiB ≈ 12.2 EiB |
| skip depth | ~32 |
| journal SSD | low single-digit TB per storage server |
| HDD random reads | ~100/s |
| miss budget | 100k VMs × 1 miss/s = 1,000 drives of IOPS |

Invariants: segment bytes never change; every VM operation is one root commit; index and counts change only by journaled steps; content naming makes reads self-verifying; CDC dedup is conditional on measured duplicate fractions.

### Lesson for our design

Two things here change how the study should be framed.

First, the closing sentence on dedup: **"CDC dedup is a background compaction flavor, adopted if measured duplicate fractions justify it."** segstore gets its capacity wins from forks sharing base bytes (structural dedup via roots) and treats content-defined chunking as an optional add-on that must earn its place by measurement. Our design leads with CDC dedup. The study's capacity result (item 2) should therefore separate the two: how much of the win comes from fork or clone sharing (which a plain CoW system also gets) versus from content-level dedup across independently written data. ZFS dedup is the right baseline for the second; ZFS clones are the baseline for the first.

Second, the read-miss arithmetic: "measure the miss rate before trusting it." Our per-host RAM cache and remote cold read are the equivalent of segstore's compute read cache and SSD tier; the number that sizes them is the miss rate under a real workload, and study item (3) should report per-transport cost multiplied by a measured miss rate, rather than the per-request latency alone.

The "same move, four names" list is a compact statement of what our offset-to-hash map buys: migration moves it, snapshot names it, restore points at it, and incremental backup is a have() set difference. Those are cheap demonstrations for the paper once the map is a first-class root.

The 100k-VM arithmetic does not apply at two hosts, but its shape does: for every capacity or throughput claim, write the one-line formula and the constant it depends on.

---

## Cross-cutting summary for the two-host design

1. **Durability class.** Our local-ack write path is segstore's *local class* ("dies with the host by contract"). Node death is not migration-minus-RAM for us; the undrained staging log is lost. State RPO = compaction interval and measure it. (Lessons 20, 26, 27)
2. **Two hosts means mirror or stall.** segstore's own two-box code is 4+4 at 2×. For us: k=2 survives a host outage, k=1 stalls cold reads and halves cost. Run both. (Lesson 27)
3. **Three collectors, not one.** Refcounts (O(1) reclaim on delete), a copier with emptiest-first and garbage-ratio trigger, and mark-and-sweep as slow ground truth. Ours has only the sweep; overwrite-heavy workloads will leak between sweeps and show up in the capacity numbers. Add refcounts as derived state and keep the sweep as auditor. Increment before, decrement after; too-high is safe, too-low is fatal. (Lessons 23, 24)
4. **The daemon is the copier.** Because we own the offset-to-hash map (the "conventional disk" lane), the host can see liveness and must compact; no guest patch needed. Trigger on garbage ratio, not free space; enable guest discard. (Lesson 24)
5. **Two-level naming.** Pack chunks into a fixed-size container that is the unit of placement, replication, repair and reclaim; keep chunk-level entries in per-container manifests, not in the RAM index. Forward old container names on repack instead of rewriting maps. (Lessons 15, 22, 24)
6. **The map is the root record and the one irreplaceable thing.** Journal and checkpoint it with a checksummed root pointer; make the chunk store self-describing so the hash index is rebuildable by scan; the RAM cache is authoritative for nothing. (Lesson 22)
7. **Forks need parent pointer plus delta, tombstones, and content-addressed flattened maps.** A flat-map copy per fork is O(disk). (Lesson 19)
8. **No cross-VM page sharing on a block lane.** A base image via virtio-blk is cached once per guest; only the daemon cache dedups. Scope RAM claims accordingly, and never scan for identical pages. (Lesson 16)
9. **Hash is a name, not a capability; dedup domain is part of the key.** Single-tenant is an assumption to state; have() must never be guest-reachable. (Lesson 17)
10. **Back-pressure is a designed path.** Chunk store full → compactor stalls → log watermark → ENOSPC to guest. Deletes need no allocation. Name the retention metric for the log. (Lessons 18, 27)
11. **Cold reads are seeks, not bytes.** Pack in write order, prefetch whole containers on migration, and measure per-transport cost as (miss rate × per-miss cost) plus whole-container sequential fetch. Measure link bandwidth, not nominal. (Lessons 20, 27, 28)
12. **Single writer per root, enforced at migration by an atomic ownership swap.** Peers pass names or chunk bodies, never proxy VM I/O. (Lesson 25)
13. **Separate structural dedup (clones) from content dedup (CDC) in the capacity study.** segstore treats CDC as optional pending measured duplicate fraction. (Lesson 28)
14. **Reservation before stream and index-driven repair** apply even at two hosts; anti-affinity, box maintenance windows, and the EC code table do not beyond "two failure domains." (Lesson 21)
