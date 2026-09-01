<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
	import { Diagram, Node, Edge, Group } from '$lib/components/diagram';
</script>

<PageHead num="03" />
<p class="lede">
	<mark>Phase 3 runs only if the census opens the week-6 gate.</mark>
	It builds the minimum backend that prices the CDC-only leaf on a guest block path, and the
	paper labels its numbers preliminary.
	Two weeks is not enough to do this well; the design below is scoped to what two weeks can
	measure and the rest is the next study.
</p>

<h2>When it runs, and what it is for</h2>
<p>
	If block-capturable at 4K is below 90% of cross-lineage on the VM corpora, or if the model and
	Nix corpora show a CDC-only leaf large enough that an operator of those classes would want it
	priced, the instrument is built.
	Its one job is to add a fifth rung, R4, to the page 02 table: the same workloads, the same
	metrics, and a capture figure the census predicts in advance.
	If R4's capture lands near the census's CDC-plus-aligned ceiling at latency parity, the residue
	is reachable at a stated cost; if it does not, the gap is the finding.
</p>
<p>
	The design is deliberately conventional so that every number is attributable to content-defined
	chunking on a block path and not to an implementation trick.
	Post-process dedup ships in Windows Server (El-Shimi et al., ATC '12) and in Ceph's chunk pool
	(TiDedup, ATC '23); OpenZFS 2.3's fast dedup already puts a log ahead of its dedup table and
	flushes it sorted; the append-only store with a rebuildable index is Venti; the compaction
	pattern is the LSM tree's.
	Nothing here is new and the paper says so.
</p>

<h2>Datapath</h2>
<p>
	<mark>An LSM tree whose compaction step is content addressing.</mark>
	The guest sees a virtio-blk device on stock QEMU, connected over vhost-user-blk to an external
	process, the daemon; all new code lives there.
	Guest memory is shared with the daemon, so requests are read in place.
	The daemon issues storage IO through io_uring.
</p>

<figure>
	<svg
		viewBox="0 0 1000 400"
		role="img"
		aria-label="The two-tier datapath: a guest in stock QEMU reaches the daemon over vhost-user; writes append to a durable staging log; a background compactor moves unique chunks into the capacity tier of map, index, and chunk store; reads check staging first."
		style="max-width: 100%; height: auto;"
	>
		<defs>
			<marker id="arr" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
				<polygon points="0,0 8,4 0,8" fill="currentColor" />
			</marker>
		</defs>

		<!-- stock QEMU with guest -->
		<rect x="15" y="105" width="150" height="120" rx="4" fill="none" stroke="currentColor" stroke-width="1.25" />
		<text x="90" y="133" text-anchor="middle" font-size="12" font-weight="600" fill="currentColor">stock QEMU</text>
		<rect x="35" y="150" width="110" height="55" rx="4" fill="none" stroke="currentColor" stroke-width="1" opacity="0.8" />
		<text x="90" y="173" text-anchor="middle" font-size="11" fill="currentColor" opacity="0.8">guest</text>
		<text x="90" y="192" text-anchor="middle" font-size="10.5" fill="currentColor" opacity="0.6">virtio-blk</text>

		<!-- vhost-user link -->
		<line x1="165" y1="165" x2="296" y2="165" stroke="currentColor" stroke-width="1.25" marker-end="url(#arr)" />
		<text x="230" y="153" text-anchor="middle" font-size="10.5" fill="currentColor">vhost-user-blk</text>
		<text x="230" y="185" text-anchor="middle" font-size="10" fill="currentColor" opacity="0.6">shared guest memory</text>

		<!-- daemon -->
		<rect x="300" y="120" width="150" height="90" rx="4" fill="none" stroke="currentColor" stroke-width="1.25" />
		<text x="375" y="158" text-anchor="middle" font-size="12" font-weight="600" fill="currentColor">daemon</text>
		<text x="375" y="178" text-anchor="middle" font-size="11" fill="currentColor" opacity="0.6">all new code · io_uring</text>

		<!-- staging log (durability boundary, accent) -->
		<rect x="560" y="120" width="160" height="90" rx="4" fill="none" stroke="#d97706" stroke-width="1.5" />
		<text x="640" y="152" text-anchor="middle" font-size="12" font-weight="600" fill="currentColor">staging log</text>
		<text x="640" y="171" text-anchor="middle" font-size="11" fill="currentColor" opacity="0.6">append-only · NVMe</text>
		<text x="640" y="194" text-anchor="middle" font-size="10.5" fill="#d97706">FLUSH → fdatasync → ack</text>

		<!-- write / fresh read between daemon and staging -->
		<line x1="450" y1="150" x2="556" y2="150" stroke="currentColor" stroke-width="1.25" marker-end="url(#arr)" />
		<text x="505" y="140" text-anchor="middle" font-size="11" fill="currentColor">write · append</text>
		<line x1="560" y1="188" x2="454" y2="188" stroke="currentColor" stroke-width="1.25" marker-end="url(#arr)" />
		<text x="505" y="206" text-anchor="middle" font-size="11" fill="currentColor">read · fresh</text>

		<!-- compactor -->
		<rect x="560" y="290" width="160" height="70" rx="4" fill="none" stroke="currentColor" stroke-width="1.25" />
		<text x="640" y="318" text-anchor="middle" font-size="12" font-weight="600" fill="currentColor">compactor</text>
		<text x="640" y="338" text-anchor="middle" font-size="11" fill="currentColor" opacity="0.6">FastCDC · BLAKE3</text>
		<line x1="640" y1="210" x2="640" y2="286" stroke="currentColor" stroke-width="1.25" marker-end="url(#arr)" />
		<text x="652" y="254" text-anchor="start" font-size="11" fill="currentColor">settled extents</text>

		<!-- capacity tier -->
		<rect x="790" y="60" width="195" height="320" rx="6" fill="none" stroke="currentColor" stroke-width="1" opacity="0.35" />
		<text x="802" y="82" text-anchor="start" font-size="10" letter-spacing="0.08em" fill="currentColor" opacity="0.6">CAPACITY TIER</text>

		<rect x="815" y="95" width="145" height="52" rx="4" fill="none" stroke="currentColor" stroke-width="1.25" />
		<text x="887" y="117" text-anchor="middle" font-size="12" font-weight="600" fill="currentColor">map</text>
		<text x="887" y="136" text-anchor="middle" font-size="10.5" fill="currentColor" opacity="0.6">offset → hash</text>

		<rect x="815" y="180" width="145" height="52" rx="4" fill="none" stroke="currentColor" stroke-width="1.25" />
		<text x="887" y="202" text-anchor="middle" font-size="12" font-weight="600" fill="currentColor">index</text>
		<text x="887" y="221" text-anchor="middle" font-size="10.5" fill="currentColor" opacity="0.6">hash → location · RAM</text>

		<rect x="815" y="295" width="145" height="60" rx="4" fill="none" stroke="currentColor" stroke-width="1.25" />
		<text x="887" y="320" text-anchor="middle" font-size="12" font-weight="600" fill="currentColor">chunk store</text>
		<text x="887" y="339" text-anchor="middle" font-size="10.5" fill="currentColor" opacity="0.6">append-only</text>

		<!-- compactor -> store -->
		<line x1="720" y1="325" x2="811" y2="325" stroke="currentColor" stroke-width="1.25" marker-end="url(#arr)" />
		<text x="766" y="313" text-anchor="middle" font-size="10.5" fill="currentColor">unique chunks</text>

		<!-- cold read: daemon -> map -> index -> store -->
		<polyline points="400,120 400,50 887,50 887,91" fill="none" stroke="currentColor" stroke-width="1.25" marker-end="url(#arr)" />
		<text x="640" y="40" text-anchor="middle" font-size="11" fill="currentColor">read · cold</text>
		<line x1="887" y1="147" x2="887" y2="176" stroke="currentColor" stroke-width="1.25" marker-end="url(#arr)" />
		<text x="897" y="166" text-anchor="start" font-size="10.5" fill="currentColor">hash</text>
		<line x1="887" y1="232" x2="887" y2="291" stroke="currentColor" stroke-width="1.25" marker-end="url(#arr)" />
		<text x="897" y="266" text-anchor="start" font-size="10.5" fill="currentColor">location</text>

		<!-- garbage sweep (dashed) -->
		<polyline points="960,121 975,121 975,325 964,325" fill="none" stroke="currentColor" stroke-width="1" stroke-dasharray="4 4" marker-end="url(#arr)" />
		<text x="975" y="262" text-anchor="middle" font-size="10.5" fill="currentColor" opacity="0.8">sweep</text>
	</svg>
	<figcaption>
		The two-tier datapath. Stock QEMU hands requests to the daemon over vhost-user-blk; writes
		append to the staging log and are durable at FLUSH; the compactor cuts settled extents with
		FastCDC, hashes with BLAKE3, and moves unique chunks into the capacity tier. Reads check
		staging first, then fall through map → index → chunk store. Dashed: mark-and-sweep reclaims
		dead chunks by hole punch.
	</figcaption>
</figure>

<h2>Write tier</h2>
<p>
	Guest writes append at block granularity to a staging log on NVMe.
	The hot path performs neither hashing nor chunking, so large writes proceed at
	sequential-append speed.
	FLUSH is fdatasync of the staging log, then the acknowledgment (A4).
	Durability belongs to the log alone, which makes the write buffer disk-backed, durable, and
	bounded.
</p>

<h2>Compactor</h2>
<p>
	A background pass reads settled extents from staging, cuts them with content-defined chunking
	(FastCDC, at the size the census found best), hashes each chunk with BLAKE3, writes unique chunks
	to the capacity tier, and updates the map.
	Extents overwritten in staging are never compacted; superseded chunks become garbage.
	CDC runs only here: on the hot path, a single offset-aligned write can move the content-defined
	boundaries of its neighborhood, so inline CDC on a block device is incoherent rather than merely
	slow.
</p>
<p>
	The same boundary problem returns in compaction, weaker.
	A settled extent is a window into the image, and a chunker started at its edge cuts chunks that
	depend on where the window fell.
	The compactor therefore re-chunks from the last settled chunk boundary before the dirty extent
	to the first boundary after it that agrees with the existing cut, the standard resynchronization
	rule.
	The census computes CDC both whole-image and extent-wise (page 01), and the gap between the two
	is this rule's price.
</p>
<p>
	The design buys its write path with two known costs, both measured.
	Write amplification: every surviving byte is written at least twice, staging then chunk store,
	plus map-journal traffic.
	Interference: compaction reads staging and writes the store on the device the guest is using;
	guest p99 is measured with the compactor active and idle and the delta reported.
</p>

<Diagram
	w={960}
	h={330}
	label="Hot path and compaction. On the hot path a guest write appends to the staging log and is acknowledged; a guest FLUSH becomes an fdatasync of the log and a durable acknowledgment. Below, in the background, settled extents are re-chunked from a synchronized boundary with FastCDC and hashed with BLAKE3; the hash is looked up in the index; on a hit the map points at the existing chunk, on a miss the chunk is appended to the store and the index updated, then the map points at it."
	caption="Hot path and compaction. The hot path never hashes; the hit/miss decision, where dedup happens, sits off the guest's latency path."
>
	<Group x={20} y={20} w={920} h={110} label="hot path · never hashes" />
	<Node x={40} y={48} w={130} title="guest write" />
	<Edge points={[[170, 70], [210, 70]]} />
	<Node x={210} y={48} w={170} title="append to staging log" tone="outline" />
	<Edge points={[[380, 70], [420, 70]]} />
	<Node x={420} y={48} w={70} title="ack" tone="muted" />
	<Node x={560} y={48} w={120} title="guest FLUSH" />
	<Edge points={[[680, 70], [720, 70]]} />
	<Node x={720} y={48} w={200} title="fdatasync staging log" sub="then ack · durable" tone="outline" />

	<Edge points={[[295, 92], [295, 190]]} dashed label="settled" labelSeg={0} labelDx={28} labelDy={4} tone="muted" />

	<Group x={20} y={160} w={920} h={150} label="background compaction" />
	<Node x={40} y={190} w={190} h={52} title="settled extents" sub="resync at chunk boundaries" />
	<Edge points={[[230, 216], [262, 216]]} />
	<Node x={262} y={190} w={150} h={52} title="FastCDC · BLAKE3" sub="chunk · hash" />
	<Edge points={[[412, 216], [440, 216]]} />
	<Node x={440} y={200} w={130} h={32} kind="question" title="hash in index?" tone="accent" />
	<Edge points={[[570, 216], [620, 216], [620, 197], [700, 197]]} label="hit" labelSeg={0} labelDy={-8} tone="accent" />
	<Edge points={[[505, 232], [505, 277], [700, 277]]} label="miss" labelSeg={0} labelDx={20} labelDy={4} tone="accent" />
	<Node x={700} y={175} w={220} title="map points at chunk" sub="hit · nothing written" tone="accent" />
	<Node x={700} y={255} w={220} title="append to chunk store" sub="miss · insert index" />
	<Edge points={[[810, 255], [810, 219]]} tone="muted" />
</Diagram>

<h2>Capacity tier</h2>
<p>
	Three structures. The chunk store is an append-only log of records (length, hash, flags, bytes)
	and is authoritative. The index maps hash to location, resides in RAM, is rebuilt by scanning
	the store, and is never authoritative; its bytes-per-TB constant feeds the A2 extrapolation. The
	map, one per image, is a journaled offset tree from disk offset to chunk hash.
	One map structure, not two: a second map arm would price a metadata delta that sits inside
	measurement noise on every metric this study reports.
</p>

<h2>Read path</h2>
<p>
	Reads check staging, then the map, then the store. Fresh data is served from the raw log without
	indirection. Settled data pays the map walk, the index lookup, and, as the store fragments, seek
	amplification. This is the workload the design is worst at, and the page 02 boot storm and fleet
	replay hit it directly.
	In the O_DIRECT arm there is no page cache, so the daemon keeps its own chunk cache keyed by
	hash, and the cache metric on page 02 is a claim about that cache.
</p>

<h2>Crash consistency</h2>
<p>
	Two logs exist, staging and the map journal, and they must agree after a crash. Ordering rule:
	<mark>staging is senior</mark>. Compaction is idempotent (re-chunking the same extents yields the
	same hashes), and every compaction batch carries an epoch number recorded in both logs. On
	recovery: replay the staging log; discard map-journal records from any epoch whose staging
	extents were not yet marked compacted; re-run compaction from the oldest incomplete epoch.
	<code>kill -9</code> at any point, followed by this replay, must pass <code>fio --verify</code>
	before any R4 number is reported. A store that loses data measures nothing.
</p>

<h2>Chunking debt and garbage</h2>
<p>
	Staging is finite. If sustained ingest exceeds compaction bandwidth, staged bytes accumulate
	until back-pressure throttles the guest; the ceiling and the back-pressure point are measured.
	A chunk is live if staging or any map references it; collection is mark-and-sweep over the maps
	with <code>FALLOC_FL_PUNCH_HOLE</code> over dead records, no reference counts.
	In phase 3 the sweep is implemented and run once after the fleet replay to report reclaimed
	bytes; concurrent collection is out of scope.
</p>

<h2>Host filesystem</h2>
<p>
	The daemon's files reside on the same XFS as R0, R2, and R3, opened with O_DIRECT in the
	media-honest arm. XFS is required: hole punching with extent-based allocation, working O_DIRECT,
	and io_uring semantics. ZFS never sits under the daemon; stacking two copy-on-write systems would
	confound every measurement.
</p>

<figure>
	<svg
		viewBox="0 0 1000 380"
		role="img"
		aria-label="Block pointers versus chunk pointers. Left: two images share records only along a clone relationship; independently written identical bytes are stored twice unless a refcounted dedup table, bolted on beside the tree, catches them at an aligned record boundary. Right: two maps point at chunks by hash, so identical bytes are stored once wherever they came from."
		style="max-width: 100%; height: auto;"
	>
		<defs>
			<marker id="arr2" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
				<polygon points="0,0 8,4 0,8" fill="currentColor" />
			</marker>
		</defs>

		<!-- ============ LEFT: block pointers ============ -->
		<text x="240" y="28" text-anchor="middle" font-size="12" font-weight="600" fill="currentColor">block pointers</text>
		<text x="240" y="46" text-anchor="middle" font-size="10.5" fill="currentColor" opacity="0.6">identity = offset · sharing by clone lineage</text>

		<!-- roots -->
		<rect x="80" y="70" width="120" height="44" rx="4" fill="none" stroke="currentColor" stroke-width="1.25" />
		<text x="140" y="97" text-anchor="middle" font-size="11.5" fill="currentColor">image A tree</text>
		<rect x="280" y="70" width="120" height="44" rx="4" fill="none" stroke="currentColor" stroke-width="1.25" />
		<text x="340" y="97" text-anchor="middle" font-size="11.5" fill="currentColor">image B tree</text>
		<line x1="200" y1="92" x2="276" y2="92" stroke="currentColor" stroke-width="1" stroke-dasharray="4 4" marker-end="url(#arr2)" />
		<text x="238" y="82" text-anchor="middle" font-size="10" fill="currentColor" opacity="0.8">clone</text>

		<!-- fixed records row -->
		<g>
			<rect x="45" y="250" width="70" height="46" rx="3" fill="none" stroke="currentColor" stroke-width="1.25" />
			<text x="80" y="277" text-anchor="middle" font-size="10.5" fill="currentColor">r0</text>
			<rect x="125" y="250" width="70" height="46" rx="3" fill="none" stroke="currentColor" stroke-width="1.25" />
			<text x="160" y="277" text-anchor="middle" font-size="10.5" fill="currentColor">r1</text>
			<rect x="205" y="250" width="70" height="46" rx="3" fill="none" stroke="#d97706" stroke-width="1.5" />
			<text x="240" y="277" text-anchor="middle" font-size="10.5" fill="currentColor">r2</text>
			<rect x="285" y="250" width="70" height="46" rx="3" fill="none" stroke="currentColor" stroke-width="1.25" />
			<text x="320" y="277" text-anchor="middle" font-size="10.5" fill="currentColor">r3′</text>
			<rect x="365" y="250" width="70" height="46" rx="3" fill="none" stroke="#d97706" stroke-width="1.5" />
			<text x="400" y="277" text-anchor="middle" font-size="10.5" fill="currentColor">r4</text>
		</g>

		<!-- A -> records -->
		<line x1="115" y1="114" x2="83" y2="246" stroke="currentColor" stroke-width="1" marker-end="url(#arr2)" />
		<line x1="135" y1="114" x2="157" y2="246" stroke="currentColor" stroke-width="1" marker-end="url(#arr2)" />
		<line x1="160" y1="114" x2="234" y2="246" stroke="currentColor" stroke-width="1" marker-end="url(#arr2)" />
		<!-- B -> records: shares r0,r1 via clone; own r3'; own duplicate r4 -->
		<line x1="305" y1="114" x2="95" y2="248" stroke="currentColor" stroke-width="1" opacity="0.55" marker-end="url(#arr2)" />
		<line x1="320" y1="114" x2="168" y2="248" stroke="currentColor" stroke-width="1" opacity="0.55" marker-end="url(#arr2)" />
		<line x1="345" y1="114" x2="323" y2="246" stroke="currentColor" stroke-width="1" marker-end="url(#arr2)" />
		<line x1="365" y1="114" x2="397" y2="246" stroke="currentColor" stroke-width="1" marker-end="url(#arr2)" />

		<!-- duplicate callout -->
		<path d="M 240 296 L 240 318 L 400 318 L 400 296" fill="none" stroke="#d97706" stroke-width="1" />
		<text x="320" y="338" text-anchor="middle" font-size="10.5" fill="#d97706">identical bytes · stored twice, unless aligned and the DDT is on</text>

		<!-- DDT bolt-on -->
		<rect x="45" y="150" width="120" height="44" rx="4" fill="none" stroke="currentColor" stroke-width="1" stroke-dasharray="4 4" />
		<text x="105" y="169" text-anchor="middle" font-size="10.5" fill="currentColor" opacity="0.8">dedup table</text>
		<text x="105" y="185" text-anchor="middle" font-size="10" fill="currentColor" opacity="0.6">refcounts · aligned records only</text>

		<!-- divider -->
		<line x1="500" y1="30" x2="500" y2="350" stroke="currentColor" stroke-width="1" opacity="0.25" />

		<!-- ============ RIGHT: chunk pointers ============ -->
		<text x="760" y="28" text-anchor="middle" font-size="12" font-weight="600" fill="currentColor">chunk pointers</text>
		<text x="760" y="46" text-anchor="middle" font-size="10.5" fill="currentColor" opacity="0.6">identity = hash · sharing wherever content coincides</text>

		<!-- maps -->
		<rect x="600" y="70" width="120" height="44" rx="4" fill="none" stroke="currentColor" stroke-width="1.25" />
		<text x="660" y="97" text-anchor="middle" font-size="11.5" fill="currentColor">image A map</text>
		<rect x="800" y="70" width="120" height="44" rx="4" fill="none" stroke="currentColor" stroke-width="1.25" />
		<text x="860" y="97" text-anchor="middle" font-size="11.5" fill="currentColor">image B map</text>

		<!-- variable chunks row -->
		<g>
			<rect x="565" y="250" width="88" height="46" rx="3" fill="none" stroke="currentColor" stroke-width="1.25" />
			<text x="609" y="277" text-anchor="middle" font-size="10.5" fill="currentColor">c0</text>
			<rect x="663" y="250" width="52" height="46" rx="3" fill="none" stroke="currentColor" stroke-width="1.25" />
			<text x="689" y="277" text-anchor="middle" font-size="10.5" fill="currentColor">c1</text>
			<rect x="725" y="250" width="110" height="46" rx="3" fill="none" stroke="#d97706" stroke-width="1.5" />
			<text x="780" y="277" text-anchor="middle" font-size="10.5" fill="currentColor">c2</text>
			<rect x="845" y="250" width="66" height="46" rx="3" fill="none" stroke="currentColor" stroke-width="1.25" />
			<text x="878" y="277" text-anchor="middle" font-size="10.5" fill="currentColor">c3</text>
		</g>

		<!-- A -> chunks -->
		<line x1="630" y1="114" x2="606" y2="246" stroke="currentColor" stroke-width="1" marker-end="url(#arr2)" />
		<line x1="655" y1="114" x2="685" y2="246" stroke="currentColor" stroke-width="1" marker-end="url(#arr2)" />
		<line x1="680" y1="114" x2="768" y2="246" stroke="currentColor" stroke-width="1" marker-end="url(#arr2)" />
		<!-- B -> chunks: shares c0, c2 (the independent duplicate!), own c3 -->
		<line x1="825" y1="114" x2="618" y2="248" stroke="currentColor" stroke-width="1" opacity="0.55" marker-end="url(#arr2)" />
		<line x1="850" y1="114" x2="788" y2="246" stroke="currentColor" stroke-width="1" marker-end="url(#arr2)" />
		<line x1="875" y1="114" x2="877" y2="246" stroke="currentColor" stroke-width="1" marker-end="url(#arr2)" />

		<!-- shared callout -->
		<path d="M 725 296 L 725 318 L 835 318 L 835 296" fill="none" stroke="#d97706" stroke-width="1" />
		<text x="780" y="338" text-anchor="middle" font-size="10.5" fill="#d97706">identical bytes · stored once</text>

		<!-- by-hash label -->
		<text x="760" y="180" text-anchor="middle" font-size="10" fill="currentColor" opacity="0.6">pointers by hash · no refcounts · liveness by map scan</text>
	</svg>
	<figcaption>
		Block pointers versus chunk pointers. The left structure shares what was copied, plus what
		its dedup table finds at aligned record boundaries; the right shares what is equal.
	</figcaption>
</figure>

<h2>Distribution, in one paragraph</h2>
<p>
	Chunks are immutable and named by content, so placement is a function of the name and the
	capacity tier spreads across hosts without shared allocation state.
	HYDRAstor shipped this in 2009 and Ceph's chunk pool does it today; it is a property the
	instrument inherits, not a claim this study makes.
	The write path never crosses the network because staging is local, and content addressing
	becomes global at compaction, which is already asynchronous.
	What stays hard (mutable maps that follow their writer, global liveness, remote cold reads inside
	guest latency, a transport choice between kernel TCP, nvme-tcp, and QUIC) is the follow-on
	study, and it needs this study's measurement method before it can be designed.
</p>

<h2>Provenance</h2>
<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Component</th><th>Source</th><th>License</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">Hypervisor</td><td>stock <a href="https://www.qemu.org">QEMU</a>, unmodified (vhost-user-blk front end)</td><td>GPL-2.0</td></tr>
			<tr><td class="k">vhost-user protocol handling</td><td><a href="https://github.com/rust-vmm">rust-vmm</a> <code>vhost-user-backend</code>, <code>vm-memory</code>, <code>virtio-queue</code> crates; Cloud Hypervisor's <code>vhost_user_block</code> read as the reference backend</td><td>Apache-2.0</td></tr>
			<tr><td class="k">vhost-user hop, bounded once</td><td>stock <code>qemu-storage-daemon</code>, <code>--export type=vhost-user-blk</code> over the R0 file, run beside R0 so the hop's cost is known before R4 is read</td><td>GPL-2.0</td></tr>
			<tr><td class="k">Hashing</td><td>official <a href="https://github.com/BLAKE3-team/BLAKE3">blake3</a> crate</td><td>Apache-2.0/CC0</td></tr>
			<tr><td class="k">Content-defined chunking</td><td><a href="https://crates.io/crates/fastcdc">fastcdc</a> crate, shared with the census pipeline</td><td>MIT</td></tr>
			<tr><td class="k">Staging log, compactor, chunk store, index, map, sweep</td><td>written for this study</td><td>new code</td></tr>
		</tbody>
	</table>
</div>
<p>
	Because the hypervisor is unmodified, no result can be an artifact of a patched QEMU, and R0
	runs the identical binary.
	The protocol plumbing comes from maintained crates, so the two weeks are spent on the five
	components the rung is about.
</p>

<h2>Repository</h2>
<pre>{`cas/
  census/          # phase 1: pipeline binary, corpus scripts, donor protocol, decomposition
  harness/         # phase 2: rung configs R0-R3 (+R4), fio jobs, workloads, runner
  analyze/         # tables, curves, figures from ndjson; uv-run python
  results/         # tagged ndjson and figures per run
  chunkd/          # phase 3, conditional
    crates/
      daemon/      # vhost-user-blk backend: request loop, staging, FLUSH
      staging/     # append-only staging log, replay
      compact/     # FastCDC + BLAKE3 pass, resync rule, epochs, back-pressure
      store/       # chunk log, index, hole-punch sweep
      map/         # journaled offset tree
  docs/            # this spec, methodology notes`}</pre>

<PageNav num="03" />
