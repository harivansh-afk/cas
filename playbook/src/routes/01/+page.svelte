<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
	import { Diagram, Node, Edge, Group, Note } from '$lib/components/diagram';
</script>

<PageHead num="01" />
<p class="lede">
	<strong>Invariant.</strong><br />
	The network is on the read path only, only for cold chunks, and never on the write or flush path.<br />
	Every choice below serves that sentence.
</p>

<h2>One host</h2>
<p>
	The guest sees a virtio-blk device on stock QEMU.<br />
	QEMU connects it over vhost-user-blk to one process per host, the daemon.<br />
	All new code lives there.<br />
	Guest memory is shared with the daemon, so requests are read in place, and storage IO goes through io_uring.
</p>

<Diagram
	w={1000}
	h={420}
	label="The per-host datapath. A guest on stock QEMU reaches the daemon over vhost-user. Writes append to a local staging log and are acknowledged at FLUSH after fdatasync. A background compactor chunks settled extents, hashes them, and either appends unique chunks to the local store or ships them to their owner on another host, waiting for a durable ack. Reads check staging, then the local store, then the chunk cache, then fetch by name from the owner."
	caption="One host. The write path ends at the staging log. The compactor is the only thing that talks to other hosts on the write side, and it does so after the ack."
>
	<Node x={20} y={40} w={130} h={52} title="guest" sub="virtio-blk" tone="muted" />
	<Node x={20} y={112} w={130} h={44} title="stock QEMU" sub="vhost-user-blk" tone="muted" />
	<Edge points={[[150, 134], [200, 134]]} label="shared memory" labelDy={-8} />

	<Group x={200} y={20} w={780} h={380} label="daemon · one per host" tone="accent" />

	<Node x={230} y={60} w={220} h={64} title="staging log" sub={['append-only · local NVMe', 'FLUSH → fdatasync → ack']} tone="accent" />
	<Note x={340} y={145} anchor="middle" tone="muted" size={9.5} text="no hashing, no chunking, no network" />

	<Edge points={[[340, 124], [340, 190]]} label="settled extents" labelDx={64} labelDy={4} />
	<Node x={230} y={190} w={220} h={64} title="compactor" sub={['fixed 4K or FastCDC · BLAKE3', 'owner = rendezvous(hash)']} tone="outline" />

	<Edge points={[[450, 210], [520, 210]]} label="owner = self" labelDy={-8} />
	<Node x={520} y={190} w={200} h={64} title="local store" sub={['append-only chunks', 'hole-punch GC']} />
	<Edge points={[[450, 238], [520, 300]]} label="owner = peer" labelDy={14} labelDx={-10} tone="accent" />
	<Node x={520} y={280} w={200} h={64} title="PUT to owner" sub={['batched · durable ack', 'then mark compacted']} tone="accent" />
	<Edge points={[[720, 312], [960, 312]]} tone="accent" label="to host B" labelDy={-8} />

	<Node x={760} y={60} w={200} h={52} title="index" sub="hash → offset · RAM · rebuildable" />
	<Node x={760} y={130} w={200} h={52} title="chunk cache" sub="hash → bytes · RAM · bounded" tone="outline" />
	<Node x={760} y={200} w={200} h={52} title="maps" sub="offset → hash · one per image" />

	<Note x={230} y={330} tone="muted" size={10} text={['read: staging → local store → cache → GET(hash) from owner', 'prefetch: next chunks from the map on sequential reads']} />
</Diagram>

<h2>Write path</h2>
<p>
	Guest writes append at block granularity to a staging log on local NVMe.<br />
	FLUSH is <code>fdatasync</code> of the log, then the acknowledgment.<br />
	The hot path hashes nothing and chunks nothing, so large writes proceed at sequential-append speed.
</p>
<p>
	Durability belongs to the log alone.<br />
	The page cache never holds the only copy of anything, and every file is opened O_DIRECT.<br />
	Staging is finite; when ingest outruns compaction, back-pressure throttles the guest, and the point where it engages is measured.
</p>

<h2>Compactor</h2>
<p>
	A background pass reads settled extents from staging, cuts them into chunks, hashes each with BLAKE3, and drops any hash already in the local index.<br />
	Chunking is fixed 4K or FastCDC, chosen per arm on page 02.<br />
	Extents overwritten in staging are never compacted.
</p>
<p>
	For each new chunk, the owner is the first k hosts in rendezvous order of its hash.<br />
	If the owner is this host, the chunk is appended to the local store and fdatasync'd.<br />
	Otherwise it goes in a batch to the owner, which appends, fdatasyncs once per batch, and acks.<br />
	<mark>Only after the ack does the extent count as compacted.</mark><br />
	Staging is the write-ahead log for the whole fleet.
</p>
<p>
	Two costs come with this and both are measured.<br />
	Every surviving byte is written at least twice, staging then store, plus journal traffic.<br />
	Compaction reads and writes the same device the guest is using, so guest p99 is measured with the compactor active and idle.
</p>
<p class="note">
	CDC over a dirty extent re-chunks from the last settled boundary before it to the first boundary after it that agrees with the existing cut.<br />
	This is the standard resynchronization rule (LBFS locality; Xet's boundary reset), and it is why CDC never runs on the hot path: one aligned write can move every boundary in its neighborhood.
</p>

<h2>Read path</h2>
<p>
	Reads check staging, then the local store, then the chunk cache, then send <code>GET(hash)</code> to the owner.<br />
	The owner answers from its cache if the chunk is hot, otherwise from its store.<br />
	Fresh data never pays indirection; settled data pays the map walk, the index lookup, and, if the owner is remote, one round trip.
</p>
<p>
	The chunk cache is daemon-owned RAM keyed by hash, LRU, with a size that is a parameter.<br />
	Because every file is O_DIRECT, the kernel page cache is out of the picture on every host, and the cache size can be bounded equal to ARC on the ZFS rung.
</p>
<p>
	Prefetch is the daemon issuing the next D hashes from the map when it sees sequential reads, and optionally replaying a recorded boot profile.<br />
	D is swept on page 04.
</p>

<h2>Capacity tier</h2>
<p>
	The local store is an append-only log of records (length, hash, flags, bytes) and is authoritative for the chunks this host owns.<br />
	The index maps hash to offset, lives in RAM, and is rebuilt by scanning the store; its bytes per TB is the constant the chunk-size arms measure.<br />
	The map, one per image, is a journaled offset tree from disk offset to chunk hash.<br />
	It lives with the guest's host and moves when the guest does.
</p>

<h2>Protocol</h2>
<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Message</th><th>Reply</th><th>Used by</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">GET(hash)</td><td>bytes</td><td>cold read, prefetch</td></tr>
			<tr><td class="k">PUT(batch of chunks)</td><td>ack after one fdatasync</td><td>compactor shipping to an owner</td></tr>
			<tr><td class="k">HAS(hashes)</td><td>bitmap</td><td>provisioning, migration, sync</td></tr>
			<tr><td class="k">LIVE(epoch, hashes)</td><td>ack</td><td>garbage collection</td></tr>
		</tbody>
	</table>
</div>
<p>
	Length-prefixed messages over kernel TCP, one connection per core, <code>TCP_NODELAY</code>, driven by io_uring.<br />
	The daemon runs spinning or sleeping; page 04 measures both, because the wakeup is part of the price.<br />
	Rendezvous hashing means a reader already knows the owner of every hash; nobody looks up anyone else's index.
</p>
<p>
	RDMA and NVMe-oF exports appear on page 04 as probes that show what the kernel stack costs.<br />
	The architecture does not depend on either.
</p>

<h2>Placement and k</h2>
<p>
	Owner set = the first k hosts in rendezvous order of the chunk's hash.<br />
	k is the one cross-host parameter.<br />
	On two hosts, k = 2 means every chunk is on both (replicated) and k = 1 means each chunk lives on exactly one (partitioned).<br />
	Page 03 measures both; a deployment would run k ≥ 2 on N ≥ 3 hosts.
</p>

<h2>Durability</h2>
<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Failure</th><th>What survives</th><th>Against R0 and R1</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">daemon crash</td><td>everything: replay the staging log, re-run incomplete compaction epochs</td><td>gate G2, <code>fio --verify</code> after <code>kill -9</code></td></tr>
			<tr><td class="k">host crash, power loss</td><td>everything acked: FLUSH was fdatasync on local NVMe</td><td>same contract as a local disk</td></tr>
			<tr><td class="k">host lost</td><td>acked bytes not yet shipped are gone; with k = 1, chunks it owned are gone fleet-wide</td><td>R0 and R1 lose everything too; the window is measured, and k ≥ 2 closes the second half</td></tr>
		</tbody>
	</table>
</div>
<p>
	Two rules follow.<br />
	Bytes are durable on local NVMe before they go on the wire, always.<br />
	Shipping is two-phase: the owner fdatasyncs and acks before the sender marks anything compacted or reclaimable.
</p>
<p>
	The window between a local ack and the chunk being durable on its owner is the compaction lag, measured in seconds under the fleet replay.<br />
	One optional arm closes it: mirror the staging tail to the peer on every FLUSH and wait for its fdatasync before acking.<br />
	That is what every production system in this space does, and the arm measures what it costs: one round trip per FLUSH.
</p>

<h2>Crash consistency</h2>
<p>
	Two logs, staging and the map journal, must agree after a crash.<br />
	Staging is senior.<br />
	Compaction is idempotent and every batch carries an epoch recorded in both logs.<br />
	Recovery replays staging, discards map records from any epoch whose extents were not marked compacted, and re-runs compaction from the oldest incomplete epoch.<br />
	<code>kill -9</code> at any point, then this replay, must pass <code>fio --verify</code> before any number from the daemon is reported.
</p>

<h2>Garbage</h2>
<p>
	A chunk is live if any staging log or any map on any host references it.<br />
	Each host sends its owner the live set for an epoch with <code>LIVE</code>; the owner sweeps with <code>FALLOC_FL_PUNCH_HOLE</code> over dead records.<br />
	No reference counts.<br />
	The sweep runs once after the fleet replay to report reclaimed bytes; concurrent collection is out of scope.
</p>

<h2>Out of scope</h2>
<p>
	Membership changes, failure detection, rebalancing when a host joins or leaves, authentication and encryption on the wire, more than two hosts, and concurrent garbage collection.<br />
	Each is named in future work on page 05, and none of them affects a number this study reports.
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
			<tr><td class="k">staging, compactor, store, index, maps, cache, protocol, GC</td><td>this study</td><td>new code</td></tr>
		</tbody>
	</table>
</div>
<p>
	Because the hypervisor is unmodified, no result can be an artifact of a patched QEMU, and the raw-file control runs the identical binary.
</p>

<PageNav num="01" />
