<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="02" />
<p class="lede">
	<mark>Local write, global dedup.</mark>
	The write path never crosses the network: staging is a local log on the host running the
	guest, so ingest latency is a local NVMe property at any cluster size.
	Content addressing becomes global at compaction, which is already asynchronous.
</p>

<h2>What the name buys</h2>
<p>
	Chunks are immutable and named by content, so placement is a function of the name: rendezvous
	or CRUSH-style hashing from chunk hash to k owner nodes.
	The data path carries no allocation tables, rebalancing metadata, or coordinator.
</p>
<p>
	The index partitions by the same function, so the shard owning a chunk owns its index entry;
	routing and lookup are one computation.
	Any node may cache any chunk, and caches converge cluster-wide because names are global.
	Scrub is re-hash; a corrupt replica is detected by name and repaired from any peer.
</p>
<p>
	Compaction ships a chunk only if the owning shard lacks it. Cluster ingest traffic is therefore
	<mark>proportional to unique bytes, not written bytes</mark>.
</p>

<figure>
	<svg
		viewBox="0 0 1000 430"
		role="img"
		aria-label="Two symmetric hosts. On each, the guest writes to a local staging log that never touches the network; the compactor sends unique chunks to shard owners chosen by hash prefix, so only unique chunks cross the wire. The transport is an open slot with three candidates, marked phase 2."
		style="max-width: 100%; height: auto;"
	>
		<defs>
			<marker id="arr3" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
				<polygon points="0,0 8,4 0,8" fill="currentColor" />
			</marker>
		</defs>

		<!-- HOST A -->
		<rect x="15" y="30" width="420" height="280" rx="6" fill="none" stroke="currentColor" stroke-width="1" opacity="0.35" />
		<text x="30" y="52" text-anchor="start" font-size="10" letter-spacing="0.08em" fill="currentColor" opacity="0.6">HOST A</text>

		<rect x="40" y="70" width="110" height="50" rx="4" fill="none" stroke="currentColor" stroke-width="1.25" />
		<text x="95" y="99" text-anchor="middle" font-size="11.5" fill="currentColor">guest</text>

		<rect x="40" y="160" width="150" height="60" rx="4" fill="none" stroke="currentColor" stroke-width="1.25" />
		<text x="115" y="185" text-anchor="middle" font-size="11.5" font-weight="600" fill="currentColor">staging log</text>
		<text x="115" y="204" text-anchor="middle" font-size="10" fill="currentColor" opacity="0.6">local · never networked</text>
		<line x1="95" y1="120" x2="106" y2="156" stroke="currentColor" stroke-width="1.25" marker-end="url(#arr3)" />
		<text x="70" y="143" text-anchor="middle" font-size="10" fill="currentColor">write</text>

		<rect x="240" y="160" width="130" height="60" rx="4" fill="none" stroke="currentColor" stroke-width="1.25" />
		<text x="305" y="185" text-anchor="middle" font-size="11.5" font-weight="600" fill="currentColor">compactor</text>
		<text x="305" y="204" text-anchor="middle" font-size="10" fill="currentColor" opacity="0.6">route by hash prefix</text>
		<line x1="190" y1="190" x2="236" y2="190" stroke="currentColor" stroke-width="1.25" marker-end="url(#arr3)" />

		<rect x="240" y="250" width="130" height="46" rx="4" fill="none" stroke="currentColor" stroke-width="1.25" />
		<text x="305" y="270" text-anchor="middle" font-size="10.5" fill="currentColor">shard 0x00–7f</text>
		<text x="305" y="287" text-anchor="middle" font-size="10" fill="currentColor" opacity="0.6">chunks + index</text>
		<line x1="290" y1="220" x2="298" y2="246" stroke="currentColor" stroke-width="1" marker-end="url(#arr3)" />
		<text x="252" y="238" text-anchor="middle" font-size="10" fill="currentColor">local</text>

		<!-- HOST B (mirror) -->
		<rect x="565" y="30" width="420" height="280" rx="6" fill="none" stroke="currentColor" stroke-width="1" opacity="0.35" />
		<text x="970" y="52" text-anchor="end" font-size="10" letter-spacing="0.08em" fill="currentColor" opacity="0.6">HOST B</text>

		<rect x="850" y="70" width="110" height="50" rx="4" fill="none" stroke="currentColor" stroke-width="1.25" />
		<text x="905" y="99" text-anchor="middle" font-size="11.5" fill="currentColor">guest</text>

		<rect x="810" y="160" width="150" height="60" rx="4" fill="none" stroke="currentColor" stroke-width="1.25" />
		<text x="885" y="185" text-anchor="middle" font-size="11.5" font-weight="600" fill="currentColor">staging log</text>
		<text x="885" y="204" text-anchor="middle" font-size="10" fill="currentColor" opacity="0.6">local · never networked</text>
		<line x1="905" y1="120" x2="894" y2="156" stroke="currentColor" stroke-width="1.25" marker-end="url(#arr3)" />
		<text x="930" y="143" text-anchor="middle" font-size="10" fill="currentColor">write</text>

		<rect x="630" y="160" width="130" height="60" rx="4" fill="none" stroke="currentColor" stroke-width="1.25" />
		<text x="695" y="185" text-anchor="middle" font-size="11.5" font-weight="600" fill="currentColor">compactor</text>
		<text x="695" y="204" text-anchor="middle" font-size="10" fill="currentColor" opacity="0.6">route by hash prefix</text>
		<line x1="810" y1="190" x2="764" y2="190" stroke="currentColor" stroke-width="1.25" marker-end="url(#arr3)" />

		<rect x="630" y="250" width="130" height="46" rx="4" fill="none" stroke="currentColor" stroke-width="1.25" />
		<text x="695" y="270" text-anchor="middle" font-size="10.5" fill="currentColor">shard 0x80–ff</text>
		<text x="695" y="287" text-anchor="middle" font-size="10" fill="currentColor" opacity="0.6">chunks + index</text>
		<line x1="710" y1="220" x2="702" y2="246" stroke="currentColor" stroke-width="1" marker-end="url(#arr3)" />
		<text x="748" y="238" text-anchor="middle" font-size="10" fill="currentColor">local</text>

		<!-- crossing arrows: A compactor -> B shard, B compactor -> A shard -->
		<polyline points="370,175 500,175 626,268" fill="none" stroke="#d97706" stroke-width="1.25" marker-end="url(#arr3)" />
		<polyline points="630,205 500,205 374,268" fill="none" stroke="#d97706" stroke-width="1.25" marker-end="url(#arr3)" />
		<text x="500" y="163" text-anchor="middle" font-size="10.5" fill="#d97706">unique chunks only</text>

		<!-- transport slot -->
		<rect x="330" y="340" width="340" height="72" rx="4" fill="none" stroke="currentColor" stroke-width="1" stroke-dasharray="5 4" />
		<text x="500" y="362" text-anchor="middle" font-size="10.5" font-weight="600" fill="currentColor">transport · open slot · decided in phase 2</text>
		<text x="500" y="382" text-anchor="middle" font-size="10" fill="currentColor" opacity="0.75">per-core TCP lanes (i10) · nvme-tcp · QUIC, one stream per chunk</text>
		<text x="500" y="400" text-anchor="middle" font-size="10" fill="currentColor" opacity="0.5">the wire above plugs into this slot</text>
		<line x1="500" y1="336" x2="500" y2="215" stroke="currentColor" stroke-width="1" stroke-dasharray="3 4" opacity="0.5" />
	</svg>
	<figcaption>
		The wire. Writes never leave their host; compactors route unique chunks to shard owners by
		hash prefix, so cluster traffic is proportional to unique bytes. The transport is an open
		slot with three candidates; choosing among them is the follow-on study.
	</figcaption>
</figure>

<h2>What stays hard</h2>
<ul class="plain">
	<li>
		Maps are mutable and follow their writer (A5): the map lives with the guest's host and moves
		when the guest does. Data placement is a function of content; map placement stays a function
		of where the guest runs.
	</li>
	<li>
		Global liveness requires roots from every map owner. Epoch-based collection, roots gathered
		per epoch, no reclamation inside an open epoch. Designed here, validated only at two nodes.
	</li>
	<li>
		A cold read whose chunk lives remotely pays a network round trip inside guest latency.
		Staging absorbs writes and recent reads; caching absorbs part of the remainder; what is
		left is the real cost of disaggregation, measured in the follow-on study.
	</li>
	<li>
		Partitioning places the index; it does not shrink it. Cluster-scale honesty depends on the
		per-TB constants measured in S2.
	</li>
</ul>

<h2>Transport</h2>
<p>
	The transport is a phase-2 decision and the follow-on study's subject.
	Three candidates, each representing a distinct tradeoff.
	Kernel TCP with per-core connections and batched submissions is the i10 design (NSDI '20),
	which reached RDMA-class CPU efficiency without kernel bypass.
	nvme-tcp is its standardized kernel descendant and presents remote chunks as block namespaces.
	QUIC with one stream per in-flight chunk removes head-of-line blocking across concurrent
	fetches, at a userspace per-byte CPU cost.
</p>
<p>
	Choosing among them requires the per-stage measurement methodology this study builds, applied
	at the fabric, so the choice waits until it can be measured.
</p>

<h2>Scope</h2>
<p>
	This page argues H3 and demonstrates it at two nodes: placement lands chunks by hash; a fleet
	sync transfers bytes proportional to unique bytes (gate G3). Everything further is phase 2.
</p>

<PageNav num="02" />
