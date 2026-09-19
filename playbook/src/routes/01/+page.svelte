<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
	import { Diagram, Node, Edge, Group, Note } from '$lib/components/diagram';
</script>

<PageHead num="01" />
<p class="lede">
	In local class the network is on the read path only, and only for a chunk this host does not hold.<br />
	In fleet class it is also on the FLUSH (fsync) path, once per FLUSH, to only one fixed peer.
</p>

<h2 id="baseline">Agreed baseline and scope</h2>
<p>
	The C0–C5 implementation is one host daemon serving multiple private images: local durability, fixed 4 KiB chunks, a separate copying store, COW manifests, a bounded clean read cache and quiescent GC. Stock QEMU and normal guest caching remain in place. On this single host, O = D.
</p>
<p>
	The diagrams also show the later distributed design: peer GET/PUT, placement, surplus repair, migration and fleet-class journaling. Those paths are outside C0–C5. The 16 KiB/CDC compactor arms, prefetch, DAX/shared guest memory, guest-buffer zero-copy and in-place payload placement require their own experiments; the baseline starts with prefetch off.
</p>
<p>
	This page summarizes the architecture agreed in <a href="https://git.harivan.sh/harivansh-afk/cas-research/pulls/26" target="_blank" rel="noopener">PR #26</a>. The <a href="https://git.harivan.sh/harivansh-afk/cas-research/src/commit/63354b6885179daff980e03f15a15a9c449e794a/docs/storage-design.md" target="_blank" rel="noopener">C0 design</a> defines the full local contract and failure traces; <a href="https://git.harivan.sh/harivansh-afk/cas-research/src/commit/63354b6885179daff980e03f15a15a9c449e794a/docs/storage-format.md" target="_blank" rel="noopener">storage-format.md</a> fixes the exact bytes. Linked implementation notes refine that contract. The 16 MiB manifest-page cache below is one such later implementation choice.
</p>

<h2 id="implementation-status">Implementation status · 14 September 2026</h2>
<div class="table-scroll">
	<table class="spec overview">
		<thead><tr><th>Checkpoint</th><th>Validated</th><th>Remaining</th></tr></thead>
		<tbody>
			<tr><td class="k">C0–C3</td><td>Agreed design; build-bound validator; one-copy packed append; four queues, FLUSH ordering and retained live recovery.</td><td>Accepted development checkpoints.</td></tr>
			<tr><td class="k">C4</td><td>Store, manifests, compaction, snapshots/clones and GC. All 40 functional scenarios passed and independently verified, including six compaction crash cuts.</td><td>Final allocation audit before aggregate closure.</td></tr>
			<tr><td class="k">C5</td><td>Two private ext4/SQLite guests, retained restart and fresh boots; shared caches, scheduling, all 11 pressure stages and the consolidated 41-scenario inventory passed independent verification.</td><td>Final host/guest memory accounting and dedicated-media repetitions.</td></tr>
		</tbody>
	</table>
</div>
<p>
	The initial implementation merged in <a href="https://git.harivan.sh/harivansh-afk/cas-research/pulls/27" target="_blank" rel="noopener">PR #27</a>. Subsequent atomic increments validate scheduling, telemetry and competing workloads, including staging backpressure and a burst above measured drain. The latest-main integration passed all 41 C5 scenarios and independent verification on <code>b8399ad</code>, plus Rust and Nix/guest CI. The <a href="https://git.harivan.sh/harivansh-afk/cas-research/src/commit/63354b6885179daff980e03f15a15a9c449e794a/docs/validation/2026-09-13-c5-final.md">final Markdown receipt</a> retains earlier failures and their corrections. Final memory accounting remains open. Since that run, PRs #48, #49 and #53 changed capacity waiting, compaction preparation and read admission; the C5 suite has not been rerun on those changes. <a href="https://git.harivan.sh/harivansh-afk/cas-research/src/commit/63354b6885179daff980e03f15a15a9c449e794a/TODO.md" target="_blank" rel="noopener">TODO.md</a> tracks completion; the <a href="https://git.harivan.sh/harivansh-afk/cas-research/src/commit/63354b6885179daff980e03f15a15a9c449e794a/docs/implementation.md#current-implementation-status" target="_blank" rel="noopener">implementation status</a> lists the remaining work and evidence.
</p>
<p class="note">
	Spark/KVM checks establish development correctness under recorded failure models. Dedicated-media repetitions, physical power-loss experiments and research gates G1–G6 remain pending. This dated snapshot does not establish CI, merge or live deployment.
</p>

<h2>Components on one host</h2>

<p>
	The guest runs a normal Linux filesystem on a virtio-blk device under stock QEMU.<br />
	Guest caching remains enabled. Applications can choose guest <code>O_DIRECT</code> independently of the host IO mode.
</p>
<p>
	QEMU configures the device and passes its queues and guest-memory mappings to one daemon per host over vhost-user-blk. Mapping guest memory adds no payload copy. The daemon submits storage IO through Linux io_uring.<br />
	The device advertises a 4 KiB logical block and validates whole-block requests. Virtio addresses remain in 512-byte sectors. Host buffer alignment is checked separately.
</p>
<p>
	QEMU reconnects after a daemon restart. Recovery preserves completed writes and replays requests still in flight without overwriting newer data. Concurrent retained recovery has passed the development checkpoint; the <a href="https://git.harivan.sh/harivansh-afk/cas-research/src/commit/63354b6885179daff980e03f15a15a9c449e794a/docs/implementation.md" target="_blank" rel="noopener">implementation plan</a> distinguishes it from the preserved serial reference.
</p>

<h2>Watermark</h2>

<Diagram
	w={960}
	h={236}
	label="One image's sequence numbers, O ≤ D ≤ E. O marks owner durability; D marks durable chunks and committed manifests; E marks the durable prefix. The staging tail is (D, E]. Data in (O, E] may depend solely on this host in local class."
	caption="O tracks owner durability, D compaction and E the durable prefix. Data in (O, E] may depend solely on this host in local class."
>
	<Edge points={[[40, 60], [920, 60]]} tone="muted" />
	<Note x={920} y={46} anchor="end" tone="muted" size={10} text="sequence numbers of one image, increasing" />
	<Node x={260} y={44} w={40} h={32} title="O" tone="outline" />
	<Node x={500} y={44} w={40} h={32} title="D" tone="outline" />
	<Node x={740} y={44} w={40} h={32} title="E" tone="outline" />
	<Note x={280} y={98} anchor="middle" size={11} text={['owner-durable', 'every owner has acknowledged']} />
	<Note x={520} y={98} anchor="middle" size={11} text={['compacted', 'in a store, manifest committed']} />
	<Note x={760} y={98} anchor="middle" size={11} text={['durable prefix', 'FLUSH waits here, a snapshot cuts here']} />
	<Group x={40} y={140} w={480} h={28} label="trimmed from the staging log" />
	<Group x={520} y={140} w={240} h={28} label="staging tail, replayed" tone="accent" />
	<Group x={280} y={180} w={480} h={28} label="may depend solely on this host in local class" />
	<Note x={770} y={199} size={10} tone="muted" text="(O, E]" />
	<Note x={770} y={159} size={10} tone="accent" text="(D, E]" />
</Diagram>

<p>
	Three durability watermarks describe each image.<br />
	E is the highest sequence number with no unconfirmed append before it. In local class confirmed means on local NVMe. In fleet class it means on the journal peer too.<br />
	D is the highest sequence number whose chunks are durable in a store, at their owners or as surplus copies on this host, and whose manifest entries are committed.<br />
	O ≤ D is the highest sequence number whose chunks are durable at every owner. O equals D except while a surplus copy stands in for an unreachable owner.<br />
	FLUSH waits until E covers its captured boundary. A snapshot cuts at E. The staging log is trimmed below D; discarded regions can be reclaimed by the drive. Durable recovery replays (D, E]. A daemon-only restart also preserves completed writes beyond E while the host and device remain live. Owner confirmation alone does not protect a chunk whose only owner is the lost host.<br />
	E never skips a hole, because a maximum over confirmations forgets the append still in flight, and that is the answer that loses acknowledged data.
</p>
<p>
	The committed manifest and staging must agree after a crash. A valid durable root supplies data through D; staging supplies newer mappings. Reclaimed payload below D is not required for replay. A selected complete root with missing or corrupt durable chunks is an error.<br />
	Re-running compaction over the replayed extents yields a manifest whose every offset maps to the same bytes. It need not yield the same chunk boundaries under CDC, and the sweep reclaims the orphans of the first run.<br />
	<code>kill -9</code> at any point, then this replay, must pass <code>fio --verify</code> before any number from the daemon is reported. The log's torn tail is tested in both shapes, a shortened file and a partial record followed by preallocated zeros.<br />
	Three more cases have tests because each is a defect the author met in a prior implementation:
</p>
<ul class="plain">
	<li>an empty discard that acknowledged a sequence number nothing wrote and wedged the next FLUSH</li>
	<li>a FLUSH that must cover writes completed on any queue, checked with a multi-queue test and a negative control that shows the test can see the reordering</li>
	<li>a daemon that stops answering, which leaves the guest in D-state because virtio-blk installs no timeout handler</li>
</ul>

<h2 id="retained-recovery">Retained recovery and ownership</h2>
<p>
	The local runtime also records P, the ordered publication frontier, in QEMU's retained <code>INFLIGHT_SHMFD</code> trailer before guest completion. A completed WRITE can be beyond the last durable FLUSH. On daemon replacement, the same live guest and retained FD preserve request identities, PREPARED/ACTIVE transitions and the prefix through P. Cold recovery instead relies on synchronized durable state; retaining P is not a power-loss guarantee.
</p>
<p>
	Replay reuses original identities and mutation order. File locks prevent repair while old kernel IO owns a segment. Reads pin their captured staging/manifest view; reset, cancellation and timeout retain buffers and mappings until kernel ownership ends. A terminal storage failure publishes FAILED before IOERR and prevents later success from crossing that failure. The C0 design specifies each transition and interruption case.
</p>

<h2>Write path</h2>

<Diagram
	w={960}
	h={410}
	label="Target write path. Admission limits append bytes and requests per image and host while reserving reclamation space and IO. Guest payload is copied once into final append buffers and written directly to staging. Buffers are released after IO completion; index visibility and WRITE completion follow mutation order. QEMU configures the queues and guest-memory mappings. FLUSH waits for the local log and, in fleet class, the journal peer."
	caption="Target write path: one host payload copy into a bounded append buffer. WRITE completes after staging IO and ordered visibility; FLUSH waits for durability."
>
	<Node x={20} y={122} w={180} h={68} title="guest filesystem" sub={['page cache and dirty pages', 'virtio-blk queues']} tone="muted" />
	<Node x={20} y={300} w={180} h={52} title="stock QEMU" sub="queue and memory setup" tone="muted" />
	<Group x={250} y={20} w={400} h={366} label="daemon on host A" tone="accent" />
	<Node x={280} y={46} w={340} h={58} title="admission" sub={['per-image and host byte/request limits', 'space and IO reserved for reclamation']} tone="outline" />
	<Edge points={[[450, 104], [450, 130]]} dashed />
	<Edge points={[[200, 156], [280, 156]]} label="one copy" labelDy={-9} />
	<Node x={280} y={130} w={340} h={62} title="final append buffers" sub={['aligned; daemon-owned', 'encoding and IO use the same allocation']} tone="accent" />
	<Edge points={[[200, 326], [230, 326], [230, 214], [280, 214]]} dashed tone="muted" />
	<Note x={296} y={218} size={10} tone="muted" text="vhost-user setup" />
	<Edge points={[[450, 192], [450, 268]]} label="O_DIRECT" labelDx={48} />
	<Node x={280} y={268} w={340} h={68} title="staging log on local NVMe" sub={['one log per image', 'fdatasync covers the FLUSH boundary']} tone="muted" />
	<Note x={280} y={362} size={10} tone="muted" text="buffers released after IO completion" />
	<Edge points={[[620, 302], [710, 302]]} dashed tone="accent" label="fleet class" labelDy={-9} />
	<Node x={710} y={268} w={230} h={68} title="journal peer" sub={['append, fdatasync, acknowledge', 'in parallel with local fdatasync']} tone="ghost" />
	<Note x={710} y={85} size={10} tone="muted" text={['WRITE: ordered visibility', 'FLUSH: covered writes durable', 'chunking follows in background']} />
	<Note x={710} y={168} size={10} tone="muted" text={['host file-data cache bypassed', 'for staging and chunk-store IO']} />
</Diagram>
<p>
	Each WRITE copies guest payload once into its final append buffer. Record encoding and storage submission use the same allocation. The buffer remains owned until its IO completes.<br />
	The daemon bounds the append-buffer pool by bytes and request count, with admission limits per image. The copy count describes this host write path; guest application copies, peer transfers and later storage writes are counted separately.
</p>
<p class="note">
	An aligned guest buffer could instead feed vectored direct IO without a daemon payload copy. That path would need stable guest bytes and valid mappings until IO completion, a compatible record layout, and recovery across resets. The baseline uses one owned buffer for a stable, aligned record. <a href="https://man7.org/linux/man-pages/man7/io_uring.7.html" target="_blank" rel="noopener">Linux io_uring(7)</a>
</p>
<p>
	Guest writes append at block granularity to a staging log on local NVMe, one log per image.<br />
	Sequence numbers and log positions are assigned together. Mutation publication follows sequence order even when storage IO completes out of order; later writes wait for earlier mutations before index visibility and guest completion. The index holds block-to-log offsets and ordering metadata, with no retained payload.<br />
	A WRITE completes after its staging IO and ordered index update. A later read observes that write or a newer write to the same block.
</p>
<p>
	The device requires FLUSH support. FLUSH captures a per-image sequence boundary covering writes completed on every queue, waits for the covered appends, and calls <code>fdatasync</code> before acknowledgment. Local class waits only for the local log. Fleet class also waits for the journal peer, as defined below.<br />
	Hashing and chunking run after log durability, outside the FLUSH path.
</p>
<p>
	Staging and chunk-store payload IO use <code>O_DIRECT</code> to bypass the host file-data page cache. Direct IO still requires synchronization for persistence. Buffer addresses, file offsets and lengths satisfy the backing filesystem's direct-IO alignment requirements. <a href="https://man7.org/linux/man-pages/man2/open.2.html" target="_blank" rel="noopener">Linux open(2)</a><br />
	A waiting FLUSH starts <code>fdatasync</code> as soon as its covered appends finish, without a batching delay. Later writes wait to submit until the active sync finishes; a FLUSH with a higher boundary follows those writes in the next cohort. An idle sync every 50 ms advances durability for writes without a guest FLUSH.
</p>
<p>
	The governor limits allocated staging space per image and across the host. It reserves output space and minimum IO service for compaction and manifest commits before admitting more writes.<br />
	When append exceeds safe reclamation, admission slows before consuming that reserve. Progress deadlines and the recovery or failure action are recorded before each run. Storage errors return IOERR; lack of space does not authorize admission beyond the reserve.<br />
	The experiment records the point where pressure engages, per-guest latency and the time to drain after writes stop.
</p>

<h2 id="scheduling">Admission, scheduling and resource limits</h2>
<p>
	The agreed scheduler uses per-image FIFO admission and byte deficit round robin across images with a 1 MiB quantum. At the host, one of every four ready bulk submission opportunities is reserved for compaction; three favor demand IO, and unused opportunities can be borrowed. FLUSH/control bypass bulk work and have their own reserve. This is a service policy; guest fairness and latency still require measurement.
</p>
<div class="table-scroll">
	<table class="spec overview">
		<thead><tr><th>Bound</th><th>Development default</th></tr></thead>
		<tbody>
			<tr><td class="k">Requests / queues</td><td>1 MiB payload; 4 queues × 256 entries; 128 outstanding requests per image, 1,024 per host.</td></tr>
			<tr><td class="k">Read requests</td><td>Separate pool: 8 per image, 64 per host.</td></tr>
			<tr><td class="k">Descriptor snapshot quota</td><td>6,297,600 bytes per frontend, within foreground metadata; actual spans allocate on discovery.</td></tr>
			<tr><td class="k">Control reserve</td><td>8 requests / 64 KiB per image; 32 requests / 256 KiB per host.</td></tr>
			<tr><td class="k">Append / read buffers</td><td>Each pool: 8 MiB per image, 64 MiB per host, including retained allocations.</td></tr>
			<tr><td class="k">Clean chunk cache</td><td>256 MiB per host; read-fill LRU, no automatic write admission.</td></tr>
			<tr><td class="k">Metadata</td><td>256 MiB per host: 128 MiB foreground, 128 MiB reserved for compaction. The 16 MiB manifest-page limit is inside foreground metadata.</td></tr>
			<tr><td class="k">Segments / staging</td><td>64 MiB segments; allocated staging capped at 256 MiB per image, 1 GiB per host.</td></tr>
			<tr><td class="k">Compaction</td><td>One worker; at most 1 MiB input payload and 128 MiB new manifest pages per transaction; 100 ms settle, 1 s maximum durable-version age.</td></tr>
			<tr><td class="k">Deadlines</td><td>Capacity waits do not expire; 30 s submitted IO; 60 s recovery.</td></tr>
		</tbody>
	</table>
</div>
<p>
	Reserve R = 3S + M + 16 MiB for background progress, where S is segment size and M the manifest transaction cap: 336 MiB at these defaults. Account allocated bytes plus accepted promises. Start pressure compaction at 75% of a staging cap; stop foreground admission at the cap or before invading the disk reserve; resume below 60% when all budgets permit. Unique live data can exhaust usable capacity and return ENOSPC/IOERR. These are development settings, not measured optima; record every override.
</p>

<h2>Compactor</h2>

<Diagram
	w={960}
	h={440}
	label="The background compactor reads durable, immutable versions from the staging log on settle, age or pressure triggers. They are cut into chunks (fixed 4 KiB, fixed 16 KiB, or FastCDC snapped to 4 KiB), and are hashed with BLAKE3. Rendezvous order of the hash names the owner set, and HAS asks each owner what it lacks. A chunk this host owns is appended to the local store and fdatasynced. A chunk another host owns goes there in a sealed segment, which the owner appends, fdatasyncs, and acknowledges. If an owner is unreachable, the chunk is kept in the local store as a pinned surplus copy and a repair queue retries the send. An extent counts as compacted when its chunks are durable in a store and the manifest commit that references it is durable; the staging log is trimmed below D, and O trails D while a surplus copy stands in for an owner."
	caption="Background compaction in the copying-store baseline. New unique payload is written to a chunk store before staging is reclaimed; this is separate from the ingress copy count."
>
	<Note x={20} y={20} size={10} tone="muted" text="trigger: settle window, maximum age or staging pressure" />
	<Node x={20} y={40} w={190} h={56} title="staging log" sub="durable, immutable version" tone="muted" />
	<Edge points={[[210, 68], [250, 68]]} />
	<Node x={250} y={40} w={180} h={56} title="chunk" sub="4/16 KiB or FastCDC" tone="outline" />
	<Edge points={[[430, 68], [470, 68]]} />
	<Node x={470} y={40} w={150} h={56} title="BLAKE3" sub="one hash per chunk" tone="outline" />
	<Edge points={[[620, 68], [660, 68]]} />
	<Node x={660} y={40} w={280} h={56} title="owners = rendezvous(hash)" sub="first k hosts; HAS asks what they lack" tone="accent" />

	<Edge points={[[800, 96], [800, 130], [150, 130], [150, 170]]} />
	<Edge points={[[800, 130], [480, 130], [480, 170]]} />
	<Edge points={[[800, 96], [800, 170]]} />
	<Node x={40} y={170} w={220} h={64} title="this host owns it" sub={['append to the local store', 'fdatasync']} />
	<Node x={370} y={170} w={220} h={64} title="another host owns it" sub={['PUT a sealed segment', 'owner appends, fdatasyncs, acks']} tone="accent" />
	<Node x={680} y={170} w={240} h={64} title="owner unreachable" sub={['pinned surplus in local store', 'a repair queue retries the PUT']} tone="ghost" />

	<Edge points={[[150, 234], [150, 270], [480, 270], [480, 300]]} />
	<Edge points={[[480, 234], [480, 300]]} />
	<Edge points={[[800, 234], [800, 270], [480, 270]]} arrow={false} />
	<Node x={250} y={300} w={460} h={64} title="extent compacted" sub={['chunks durable at owners or as local surplus', 'manifest committed before staging is reclaimed']} tone="accent" />
	<Note x={20} y={401} size={10} tone="muted" text={['new unique payload is written again in this baseline', 'the governor reserves output space and IO for reclamation']} />
</Diagram>

<p>
	A background pass reads settled extents from the staging log, cuts them into chunks, hashes each with BLAKE3, and skips any hash that every current owner already holds and has fenced.<br />
	A copy in a cache does not count as held.<br />
	Chunking is fixed 4 KiB, fixed 16 KiB, or FastCDC with boundaries snapped to 4 KiB (one per measurement arm on page 02).<br />
	Normally an extent is compacted after a settle window without writes. A maximum age or staging pressure forces a pass over an immutable version even if the guest keeps overwriting that extent.<br />
	The window is a parameter, and its effect on chunk traffic is measured.<br />
	A discarded or zero-filled range names no chunk and consumes no store payload. Range updates still require index and manifest work proportional to the affected mappings. A read of such a range returns zeros.
</p>
<p>
	Rendezvous order of the hash names the k owners (Placement, below).<br />
	If this host is an owner, the chunk is appended to the local store and made durable with fdatasync.<br />
	Otherwise it goes to each owner in a sealed segment of many chunks, which the owner appends, fdatasyncs once, and acknowledges.<br />
	<mark>An extent counts as compacted at D, when its chunks are durable in a store, at their owners or as surplus copies here, and its manifest entry is committed. It counts as owner-durable at O, after every owner's acknowledgment.</mark><br />
	If an owner is unreachable, the compactor appends the chunk to the local store as a surplus copy, pinned until that owner acknowledges it later. A repair queue retries the send.<br />
	In local class, durable surplus copies and the manifest commit allow log reclamation during an owner outage while local capacity remains. Surplus copies consume the same disk budget. The sweep reclaims them after owner acknowledgment.<br />
	A chunk the compactor has produced stays pinned, in the staging log or in a store, until the manifest commit that references it is durable. An owner never reclaims a chunk it acknowledged before that fence.<br />
	The staging log is therefore the write-ahead log for every chunk this host produces, wherever the chunk might end up.
</p>
<p>
	The compactor releases append and FLUSH locks before chunk IO, manifest IO or owner RPC. A test slows the store to one second per append while the log remains healthy and checks FLUSH against its recorded deadline.
</p>
<p class="note">
	CDC over a dirty extent re-chunks from the last settled boundary before it to the first boundary after it that agrees with the existing cut.<br />
	Two published properties make the rule exact (<a href="https://pdos.csail.mit.edu/papers/lbfs:sosp01/lbfs.pdf" target="_blank" rel="noopener">LBFS</a> locality; <a href="https://huggingface.co/docs/xet/en/chunking" target="_blank" rel="noopener">Xet</a>'s boundary reset), and it is why CDC never runs on the hot path. One aligned write can move every boundary in its neighborhood.
</p>

<h2>Read path</h2>

<Diagram
	w={960}
	h={330}
	label="Reads that reach the daemon resolve fresh data through the staging index and settled data by hash. The host chunk cache is clean and bounded by bytes. Concurrent misses for one hash share a fetch; fetch and prefetch buffers and request counts are bounded. A local store miss sends GET to the first reachable owner, and the reply is verified before use."
	caption="Reads after a guest-cache miss or guest direct IO. The host cache is clean and bounded; outstanding fetches have separate byte and request limits."
>
	<Note x={20} y={24} size={10} tone="muted" text="guest cache miss or guest O_DIRECT" />
	<Node x={20} y={60} w={200} h={40} title="in the staging log?" kind="question" />
	<Edge points={[[220, 80], [250, 80]]} label="no" labelDy={-6} />
	<Node x={250} y={60} w={200} h={40} title="in the chunk cache?" kind="question" />
	<Edge points={[[450, 80], [480, 80]]} label="no" labelDy={-6} />
	<Node x={480} y={60} w={200} h={40} title="in the local store?" kind="question" />
	<Edge points={[[680, 80], [700, 80]]} label="no" labelDy={-6} tone="accent" />
	<Node x={700} y={50} w={250} h={60} title="GET(hash)" sub={['first reachable owner', 'cache first, then store']} tone="accent" />

	<Edge points={[[120, 100], [120, 170]]} label="yes" labelDx={14} />
	<Edge points={[[350, 100], [350, 170]]} label="yes" labelDx={14} />
	<Edge points={[[580, 100], [580, 170]]} label="yes" labelDx={14} />
	<Edge points={[[825, 110], [825, 170]]} tone="accent" />
	<Node x={20} y={170} w={200} h={52} title="one NVMe read" sub="staging index: offsets only" />
	<Node x={250} y={170} w={200} h={52} title="clean cache hit" sub="hash-keyed; byte limit" />
	<Node x={480} y={170} w={200} h={52} title="one NVMe read" sub="index lookup first" />
	<Node x={700} y={170} w={250} h={52} title="one round trip" sub="reply hashed before it is served" tone="accent" />

	<Note x={20} y={251} size={10} tone="muted" text={['one host cache; read fills populate it', 'same-hash misses share a fetch; fetch and prefetch buffers are bounded']} />
	<Note x={20} y={302} tone="muted" size={10} text="prefetch: on sequential reads the daemon asks for the next configured number of hashes in one GET; the guest's own readahead adds to the prefetch depth" />
</Diagram>

<p>
	A guest-cache miss or guest direct read reaches the daemon. The staging index resolves fresh data first. For settled data, the manifest supplies a hash for the chunk cache, then the local store if this host holds the chunk. Otherwise the daemon sends <code>GET</code> to the first reachable owner in rendezvous order.<br />
	The owner answers from its cache if the chunk is hot and from its store otherwise.<br />
	Every chunk that arrives over the network is hashed before it is used, so a wrong or corrupt reply is detected and never served. A record read from a store is checked against its inline checksum.<br />
	Fresh data is served without indirection. Settled data incurs the manifest lookup, the index lookup, and, if the chunk is remote, one round trip.<br />
	<code>GET</code> uses separate connections and has priority over bulk <code>PUT</code>. Disk scheduling preserves the compactor service reserved by the governor.
</p>
<p>
	One daemon-owned LRU chunk cache serves the host, keyed by hash and bounded by bytes. Read fills populate it; writes do not automatically enter it. Eviction drops a cached copy without changing durable storage.<br />
	Evicted bytes remain charged until the final reader releases them. Coalesced reads retain the original fetch credit through the final shared reader; leaders and waiters are bounded.<br />
	A fetched chunk this host does not own lives in that memory cache only. Concurrent misses for one hash share a fetch. Fetches and prefetch have bounded buffers and request counts, with demand reads served first.<br />
	A disk tier for fetched chunks, as Liquid had, is a knob measured only if time remains, since page 04 predicts a refetch from a peer's memory costs less than a local disk hit.
</p>
<p>
	Prefetch is the daemon issuing the next configured number of hashes from the manifest in one <code>GET</code> when it sees sequential reads, and optionally replaying a recorded boot profile.<br />
	The guest's own readahead is left at its default and adds to that depth.<br />
	Page 04 sweeps the prefetch depth; its parameter is separate from the publication frontier P above.
</p>

<h2>Shared guest read memory <span class="tag-stretch">proposed</span></h2>
<p>
	An optional <a href="https://github.com/qemu/qemu/blob/v10.2.4/docs/system/devices/virtio/virtio-pmem.rst" target="_blank" rel="noopener">virtio-pmem</a> frontend would expose one shared, immutable filesystem image through direct access (DAX). File-backed virtio-pmem uses the host page cache and bypasses the guest file-data cache. An <a href="https://github.com/torvalds/linux/blob/v6.18/Documentation/filesystems/overlayfs.rst" target="_blank" rel="noopener">OverlayFS</a> upper would retain normal guest caching and the same private virtio-blk write contract. Copied-up files would read from that upper. The mounted lower must remain unchanged.
</p>
<p>
	The first experiment uses a prepared local image. Mapping CAS chunks into guest address ranges, publishing new content, and fetching remote misses remain separate work. Sharing an existing image establishes frontend sharing; sharing independently produced equal content would establish the CAS-specific benefit. Page 02 defines the comparison.
</p>

<h2>Store, index, and manifest</h2>
<p>
	The baseline local store is an append-only log of records (length, hash, checksum, bytes), separate from staging, and is authoritative for the chunks this host owns. Page 02 compares this layout with retaining and indexing surviving staging payloads in place; the guest contract is the same in both.<br />
	The index maps hash to store offset, lives in memory, and is rebuilt by scanning the store without re-hashing, because the hash is inline.<br />
	Its bytes per TB is the constant the chunk-size arms measure.<br />
	In partitioned mode a host indexes the chunks it owns plus any surplus copies awaiting an owner, so per-host index memory is k/N of the fleet's once the repair queue is empty.<br />
	An index entry is added only after the data it points to is durable, at every fence.<br />
	The manifest, one per image, is a copy-on-write tree from disk offset to chunk hash, packed in offset order. A root commit becomes durable after its referenced chunks and before staging reclamation.<br />
	It lives with the guest's host and moves when the guest does.
</p>
<p>
	The local manifest uses checksummed 4 KiB COW B+tree pages, height at most eight, and fixed COMMIT pages identifying generation, root and D. Chunk data is synced before the manifest commit, and D publishes before covered staging can be reclaimed behind all read/replay pins. A shared <a href="https://git.harivan.sh/harivansh-afk/cas-research/src/commit/63354b6885179daff980e03f15a15a9c449e794a/docs/metadata-cache.md" target="_blank" rel="noopener">manifest-page cache</a> keys verified pages by incarnation, COMMIT end and offset. Cached bytes retain no root/file pins; every hit passes the checked lookup descent.
</p>
<p>
	A local snapshot pauses mutation admission, drains and FLUSHes, and compacts through the captured cut. XFS FICLONE creates an immutable standalone manifest; file and directory sync precede publication. A writable clone gets a private identity/log and a new COMMIT with D = E = O = 0 in its own sequence namespace. Catalog changes use temp-file, fsync, rename and directory fsync. Snapshot membership changes are excluded from host-wide quiescent GC; measure the pause.
</p>

<h2>Protocol <span class="tag-stretch">after C5</span></h2>
<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Message</th><th>Reply</th><th>Used by</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">GET(hashes)</td><td>bytes per hash</td><td>cold read, prefetch</td></tr>
			<tr><td class="k">PUT(segment)</td><td>ack after one fdatasync</td><td>compactor sending a sealed segment of chunks to an owner</td></tr>
			<tr><td class="k">HAS(hashes)</td><td>bitmap of hashes the owner lacks or has not fenced</td><td>compactor before PUT, so only missing chunks are sent; provisioning verification</td></tr>
			<tr><td class="k">LIVE(epoch, hashes)</td><td>ack</td><td>garbage collection</td></tr>
			<tr><td class="k">JOURNAL(image, range)</td><td>ack after fdatasync</td><td>fleet class: the appends since the last FLUSH, sent to the journal peer</td></tr>
		</tbody>
	</table>
</div>
<p>
	Messages are length-prefixed over kernel TCP with <code>TCP_NODELAY</code>, driven by io_uring.<br />
	<code>GET</code> and <code>JOURNAL</code> have their own connections and priority. <code>PUT</code> is bulk.<br />
	Every message is idempotent and named by hash or sequence number, so any of them can be retried.<br />
	The daemon runs busy-polling or blocking. Page 04 measures both, because the scheduler wakeup is part of the cost.
</p>

<h2>Placement and the parameter k</h2>
<p>
	Chunks are placed over N hosts by rendezvous hashing. Every host scores each (chunk, host) pair with one hash function, and the k highest-scoring hosts own the chunk.<br />
	Every host computes the same owner set without shared state, a ring, or a lookup, at N hash evaluations per chunk.<br />
	CRUSH's straw2 bucket is the same computation with per-host weights (NEED CITE).<br />
	When a host joins or leaves, only the chunks whose top-k set changes move.<br />
	The journal peer for fleet class is not chosen this way. A journal needs a fixed home with ordered replay, so each image names one peer at creation and keeps it.<br />
	If a migration lands the guest on its own journal peer, the image names a new peer in the same fenced swap. On two hosts the journal peer is always the other host.<br />
	k is the one multi-host parameter.<br />
	With N hosts, k = N places every chunk on every host (replicated) and k = 1 places each chunk on exactly one (partitioned). On the two-host testbed these are k = 2 and k = 1.<br />
	Page 03 measures both, and a deployment would run k ≥ 2 on N ≥ 3 hosts.
</p>

<h2>Durability classes</h2>
<p>
	Durability is a per-image class on one pipeline. The class changes who waits at FLUSH and for how long. Chunks reach the same owners either way; fleet class adds a copy of the staging tail at the journal peer until compaction catches up.<br />
	<strong>Local class</strong>, the default: FLUSH returns after fdatasync of the staging log on this host.<br />
	<strong>Fleet class</strong>: the appends since the last FLUSH are sent to the image's journal peer, which appends it to its own log and fdatasyncs. The send proceeds in parallel with this host's fdatasync, FLUSH returns after both, and FLUSHes from several images to the same peer share one round trip and one fdatasync.<br />
	Fleet class is what <a href="https://www.nutanixbible.com/4g-book-of-aos-data-io-path.html" target="_blank" rel="noopener">Nutanix AOS</a> and <a href="https://experistg.com/wp-content/uploads/2019/12/The-technology-enabling-HPE-SimpliVity-data-efficiency.pdf" target="_blank" rel="noopener">HPE SimpliVity</a> do before they acknowledge, and page 04 measures what it costs.
</p>
<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Failure</th><th>Local class</th><th>Fleet class</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">daemon crash</td><td>completed writes remain readable; recover the log and inflight requests, then re-run compaction</td><td>same</td></tr>
			<tr><td class="k">host crash, power loss</td><td>FLUSH-covered writes survive; ordinary completed writes may be lost</td><td>same</td></tr>
			<tr><td class="k">host lost</td><td>the tail (O, E] can be lost; compacted chunks survive only if another host retains a copy</td><td>the staging tail survives: the journal peer replays (D, E] onto a new host; chunks the lost host owned survive only if k ≥ 2, as in the row below</td></tr>
			<tr><td class="k">peer lost, k = 1</td><td>chunks it owned are unreadable until it returns, and lost if its disk is; a read that needs one waits or fails with an error, never returns stale bytes; writes use surplus copies while local capacity remains</td><td>the same for reads; a FLUSH waits for journal durability within its deadline or fails. The image does not silently change durability class</td></tr>
		</tbody>
	</table>
</div>
<p>
	Reclamation preserves the selected durability class. In fleet class, the journal peer retains recovery data until durable chunks and recoverable mapping metadata on surviving hosts can replace it. A local surplus copy alone cannot release that remote journal data.
</p>
<p>
	Two rules hold in both classes:
</p>
<ul class="plain">
	<li>The compactor never sends a chunk whose staging extent is not yet durable on this host.</li>
	<li>A transferred chunk replaces its local recovery copy only after the owner acknowledges durable storage and the manifest commit is durable.</li>
</ul>

<h2>Garbage collection (GC)</h2>
<p>
	A chunk is live if any manifest on any host references it, or if an in-flight compaction has pinned it. A copy in a cache is never a reference.<br />
	The baseline sweep pauses admission, drains IO and compaction, and marks active images and snapshots. Each host sends owners the complete live set for an epoch with <code>LIVE</code> before reclamation. Delete wholly dead store segments; copy live chunks from selected mixed segments into durable destinations before deleting the originals. Dead manifest pages can be hole-punched.<br />
	Refcounting is not a concept in this architecture.<br />
	ZFS frees an overwritten block when its reference count drops. This design does not, so space can leak between sweeps.<br />
	The sweep therefore runs before every capacity measurement and when disk pressure requires reclamation. Report reclaimed bytes, copied live bytes and pause duration beside the capacity number.
</p>

<h2>Provenance</h2>
<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Component</th><th>Source</th><th>License</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">hypervisor</td><td>stock QEMU, unmodified, vhost-user-blk front end</td><td>GPL-2.0</td></tr>
			<tr><td class="k">vhost-user protocol</td><td>rust-vmm <code>vhost-user-backend</code>, <code>vm-memory</code>, <code>virtio-queue</code>; Cloud Hypervisor's <code>vhost_user_block</code> read as reference</td><td>Apache-2.0 / BSD-3-Clause</td></tr>
			<tr><td class="k">hashing</td><td><code>blake3</code> crate</td><td>CC0 / Apache-2.0</td></tr>
			<tr><td class="k">chunking</td><td><code>fastcdc</code> crate</td><td>MIT</td></tr>
			<tr><td class="k">host filesystem</td><td>XFS on the dedicated NVMe, O_DIRECT, hole punching; ZFS never sits under the daemon</td><td></td></tr>
			<tr><td class="k">staging, watermark, governor, compactor, store, index, manifests, cache, protocol, journal peer, garbage collection</td><td>this study</td><td>new code</td></tr>
		</tbody>
	</table>
</div>

<PageNav num="01" />

<style>
	.overview {
		width: 100%;
		min-width: 36rem;
	}
	.overview th,
	.overview td {
		min-width: 0;
	}
	.overview th:first-child,
	.overview td:first-child {
		width: 20%;
	}
</style>
