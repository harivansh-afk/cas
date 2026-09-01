<script lang="ts">
	import { base } from '$app/paths';
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
	import Mermaid from '$lib/components/Mermaid.svelte';

	const compactionFlow = `flowchart TD
	W[guest write] --> S[append to staging log]
	S --> A[ack]
	F[guest FLUSH] --> FD[fdatasync staging log]
	FD --> FA[ack · durable]
	S -. settled .-> RE
	subgraph background compaction
		RE[read settled extents] --> CC[FastCDC chunks · BLAKE3 hash]
		CC --> IX{hash in index?}
		IX -- hit --> MP[map points at existing chunk]
		IX -- miss --> AP[append to chunk store · insert index]
		AP --> MP
	end`;
</script>

<PageHead num="01" />
<p class="lede">
	The system is <mark>an LSM tree whose compaction step is content addressing</mark>.
	Writes land in a tier that ignores content.
	A background pass moves settled data into a tier organized by nothing else.
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

<h2>A conventional design, on purpose</h2>
<p>
	Nothing in this architecture is new, and the paper says so.
	Post-process dedup ships in Windows Server (El-Shimi et al., ATC '12) and in Ceph's chunk pool
	(TiDedup, ATC '23); OpenZFS 2.3's fast dedup already puts a log ahead of its dedup table and
	flushes it sorted, which is this design's staging step applied to metadata alone; a durable
	write buffer ahead of a reduction pipeline is how VAST is built; the append-only store with a
	rebuildable index is Venti; the compaction pattern is the LSM tree's.
</p>
<p>
	The assembly is deliberate.
	A conventional design keeps every measurement attributable to the idea under test rather than
	to an implementation trick.
	The system's job is to make the comparison possible, to keep capture off the guest's write
	path, and to produce constants the literature lacks.
</p>

<h2>Datapath</h2>
<p>
	The guest sees a virtio-blk device.
	The hypervisor is stock QEMU, connected to an external process over the vhost-user-blk
	protocol; all new code lives in that process, the daemon (page 04).
	Guest memory is shared with the daemon, so requests are read in place.
	The daemon issues storage IO through io_uring.
</p>

<h2>Write tier</h2>
<p>
	Guest writes append at block granularity to a staging log on NVMe.
	The hot path performs neither hashing nor chunking, so large writes proceed at
	sequential-append speed.
</p>
<p>
	FLUSH is fdatasync of the staging log, then the acknowledgment (A4).
	The page cache may serve reads, but durability belongs to the log alone, which makes the write
	buffer disk-backed, durable, and bounded.
</p>

<h2>Compactor</h2>
<p>
	A background pass reads settled extents from staging, cuts them with content-defined chunking
	(FastCDC), hashes each chunk with BLAKE3, writes unique chunks to the capacity tier, and updates
	the map. Extents overwritten in staging are never compacted; superseded chunks become garbage.
	CDC runs only here: on the hot path, a single offset-aligned write can move the content-defined
	boundaries of its neighborhood, so inline CDC on a block device is incoherent rather than merely
	slow.
</p>
<p>
	The same boundary problem returns in compaction, weaker.
	A settled extent is a window into the image, and a chunker started at its edge cuts chunks that
	depend on where the window fell, not only on content.
	The compactor therefore re-chunks from the last settled chunk boundary before the dirty extent
	to the first boundary after it that agrees with the existing cut, the standard resynchronization
	rule; chunks in between are replaced, the rest are untouched.
	The census computes CDC both whole-image and extent-wise as the compactor will see it, and the
	gap between the two is the price of this rule.
</p>
<p>
	One decision is fixed before the census runs.
	Jin and Miller (2009) found that fixed 4K blocks capture nearly what CDC does on VM images,
	because guest filesystems align file data to blocks; if the census confirms this within five
	percentage points on the VM corpora, the compactor's headline arm becomes fixed 4K aligned
	blocks and CDC is reported as a second arm.
	Fixed blocks remove the resynchronization rule, make the chunker trivial, and make the R1
	comparison one of mechanism (post-process, refcount-free, rebuildable index) rather than of
	granularity.
	The daemon keeps CDC in either case for the model and Nix corpora, where alignment does not
	hold.
</p>
<p>
	The design buys its write path with two known costs.
	Write amplification: <mark>every surviving byte is written at least twice</mark>, staging then
	chunk store, plus map-journal traffic; the WA factor is reported as a headline result.
	Interference: compaction reads staging and writes the store on the device the guest is using;
	S2 measures guest p99 with the compactor active and idle and reports the delta.
</p>

<Mermaid
	code={compactionFlow}
	caption="Hot path and compaction. The hot path never hashes; the hit/miss decision, where dedup happens, sits off the guest's latency path."
/>

<h2>Capacity tier</h2>
<p>
	Three structures. The chunk store is an append-only log of records (length, hash, flags, bytes)
	and is authoritative. The index maps hash to location, resides in RAM, is rebuilt by scanning
	the store, and is never authoritative; its bytes-per-TB constant feeds the A2 extrapolation. The
	map, one per image, is an ordered structure from disk offset to chunk hash, journaled, with
	copy-on-write snapshots.
</p>
<p>
	<strong>Map arms.</strong> The controlled experiment inside the daemon is the map structure. R2
	uses a conventional offset tree, the same shape as a block-pointer tree, pointing at chunks. R3
	uses a <a class="term" href="{base}/00#term-merkle-paged-map">Merkle-paged map</a>: the flat offset array is divided into fixed pages, each page hashed,
	with a hash tree over the pages. Because block-map keys are dense integers, this structure
	delivers the two properties history-independent metadata is for, diffs proportional to the
	changed pages and whole-image verification by root hash, without a prolly tree's machinery. A
	prolly tree generalizes the same properties to sparse variable keys; that case arises in
	distribution metadata and is deferred to phase 2 (page 02).
</p>

<h2>Read path</h2>
<p>
	Reads check staging, then the map, then the store. Fresh data is served from the raw log without
	indirection. Settled data pays the map walk, the index lookup, and, as the store fragments, seek
	amplification. This is the workload the design is worst at: a read-heavy process over settled
	data pays the indirection on every access. S2 includes that workload deliberately, young and
	aged.
</p>
<p>
	The cache term on page 00 depends on who holds the cache.
	With the page cache, N guests reading a shared chunk occupy one entry because the chunk is one
	file range.
	In the O_DIRECT arms there is no page cache, so the daemon keeps its own chunk cache keyed by
	hash, and the sharing claim is the same claim about that cache.
	Either way it is measured, not asserted: host device reads per guest byte read during the
	N-clone boot storm, per rung.
</p>

<h2>Crash consistency</h2>
<p>
	Two logs exist, staging and the map journal, and they must agree after a crash. Ordering rule:
	<mark>staging is senior</mark>. Compaction is idempotent (re-chunking the same extents yields the same
	hashes), and every compaction batch carries an epoch number recorded in both logs. On recovery:
	replay the staging log; discard map-journal records from any epoch whose staging extents were
	not yet marked compacted; re-run compaction from the oldest incomplete epoch.
	<code>kill -9</code> at any point, followed by this replay, must pass
	<code>fio --verify</code> (gate G4).
</p>

<h2><a class="term" href="{base}/00#term-chunking-debt">Chunking debt</a></h2>
<p>
	Staging is finite. If sustained ingest exceeds compaction bandwidth, staged bytes accumulate
	until back-pressure throttles the guest. The sustainable ingest ceiling and the point where
	back-pressure engages are measured in S2. On this testbed the compactor is expected to be
	IO-bound rather than hash-bound; the measurement confirms or refutes that.
</p>

<h2>Garbage</h2>
<p>
	A chunk is live if staging or any map references it. Collection is mark-and-sweep: scan the
	maps, build a live set, punch holes (<code>FALLOC_FL_PUNCH_HOLE</code>) over dead records. No
	reference counts; refcount maintenance is the classic dedup write tax, and the maps are small
	enough to scan. No reclamation inside an open snapshot epoch.
</p>

<h2>Host filesystem</h2>
<p>
	The daemon's files (staging log, chunk store, maps, journals) reside on XFS on the dedicated
	NVMe device, opened with O_DIRECT in the media-honest arms. XFS is required, not preferred:
	garbage collection is <code>FALLOC_FL_PUNCH_HOLE</code>, so the backing filesystem must support
	hole punching with extent-based allocation, and the store needs working O_DIRECT and io_uring
	semantics. A raw partition would remove filesystem interference but removes hole punching with
	it. ZFS never sits under the daemon; stacking two copy-on-write systems would confound every
	measurement.
</p>

<h2>The rungs</h2>
<p>
	Same stock QEMU configuration for all four; only the storage behind the device varies. R0, R2,
	and R3 share one XFS filesystem on the dedicated NVMe.
</p>
<ul class="reqs">
	<li>
		<span class="rid">R0</span><strong>Raw file on XFS, through the daemon in passthrough.</strong>
		The control: the same daemon binary with staging and compaction disabled, writing through to a
		raw file, so R0 versus R2 differs only in content addressing and not in the vhost-user hop.
		The vhost-user hop itself is bounded once, by running the same workloads against stock QEMU's
		built-in raw driver and against <code>qemu-storage-daemon</code>'s vhost-user-blk export of
		the same file; both numbers are reported beside R0 and neither is a rung.
	</li>
	<li>
		<span class="rid">R1</span><strong>Raw file on a ZFS zvol, stock OpenZFS ≥ 2.3 with fast
		dedup, its own pool on the same NVMe device.</strong> The incumbent block-pointer design with
		content-hash identity.
		Fast dedup is required, not optional: it replaces the legacy DDT's random writes with a
		sorted log flush and adds a quota and pruning, and a comparison against the pre-2.3 DDT would
		be against a design its own maintainers call obsolete.
		Configuration: <code>feature@fast_dedup</code> enabled; <code>checksum=blake3, dedup=on</code>;
		<code>volblocksize</code> arms at 4K and 16K, since zvol dedup granularity is the volblocksize
		and the census's fixed-block result decides which is headline; <code>dedup_table_quota</code>
		unset and <code>zpool ddtprune</code> never run during a measurement, both recorded;
		compression off outside the labeled compression arm; DDT memory read from
		<code>zpool status -D</code> and reported in the same index-cost column as the daemon's.
		R1 is a case study rather than a controlled comparison: it differs from the daemon in kernel
		boundary, caching, and allocation, and the paper attributes cross-rung deltas accordingly.
		Patching ZFS is out of scope; a fork would consume the schedule and demonstrate nothing
		stock ZFS does not.
		If time allows, a zero-code variant runs beside it: <code>duperemove</code> over the R0 file
		on the same XFS, which is post-process fixed-block dedup by <code>FIDEDUPERANGE</code> on the
		control filesystem and bounds block-capturable capture with no kernel boundary change.
	</li>
	<li>
		<span class="rid">R2</span><strong>The daemon, offset-tree map.</strong> Chunk-level content
		addressing, conventional metadata.
	</li>
	<li>
		<span class="rid">R3</span><strong>The daemon, Merkle-paged map.</strong> Chunk-level content
		addressing, history-independent metadata.
	</li>
</ul>
<p>
	R0 versus R2 prices content addressing. R2 versus R3 prices the metadata structure. R1 anchors
	both against the deployed state of the art and, read against the census, shows how much of the
	cross-lineage segment an aligned dedup table reaches. Controlled claims are made only within
	the daemon rungs.
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

<PageNav num="01" />
