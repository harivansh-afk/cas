<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
	import { Diagram, Node, Edge, Group, Note } from '$lib/components/diagram';
</script>

<PageHead num="01" />
<p class="lede">
	The network is on the read path only, only for cold chunks, and never on the write or flush path.<br />
	Every design choice on this page follows from that invariant.
</p>

<h2>Components on one host</h2>
<p>
	The guest sees a virtio-blk device on stock QEMU.<br />
	QEMU connects it over vhost-user-blk to one process per host, the daemon, and all new code lives there.<br />
	Guest memory is shared with the daemon, so requests are read in place, and storage IO goes through io_uring.
</p>

<Diagram
	w={1000}
	h={440}
	label="The per-host datapath. A guest on stock QEMU reaches the daemon over vhost-user. Writes append to a local staging log and are acknowledged at FLUSH after fdatasync. A background compactor chunks settled extents, hashes them, and either appends unique chunks to the local store or sends them to their owner on another host, waiting for a durable ack. Reads check staging, then the local store, then the chunk cache, then fetch by hash from the owner."
	caption="One host. The write path ends at the staging log. The compactor is the only component that talks to other hosts on the write side, and it does so after the ack."
>
	<Node x={20} y={44} w={130} h={52} title="guest" sub="virtio-blk" tone="muted" />
	<Node x={20} y={116} w={130} h={44} title="stock QEMU" sub="vhost-user-blk" tone="muted" />
	<Edge points={[[150, 138], [200, 138]]} />
	<Note x={22} y={110} size={9.5} tone="muted" text="shared guest memory" />

	<Group x={200} y={20} w={780} h={400} label="daemon · one per host" tone="accent" />

	<Node x={230} y={60} w={220} h={64} title="staging log" sub={['append-only · local NVMe', 'FLUSH → fdatasync → ack (local class)']} tone="accent" />
	<Note x={470} y={86} tone="muted" size={9.5} text={['no hashing or chunking on this path', 'fleet class adds one JOURNAL round trip to a fixed peer before the ack']} />

	<Edge points={[[340, 124], [340, 200]]} />
	<Note x={352} y={166} size={10} text="settled extents" />
	<Node x={230} y={200} w={240} h={64} title="compactor" sub={['chunk, then hash with BLAKE3', 'owner = rendezvous hash of the chunk']} tone="outline" />

	<Edge points={[[470, 232], [570, 232]]} />
	<Note x={520} y={190} anchor="middle" size={10} text="owner is this host" />
	<Node x={570} y={200} w={180} h={64} title="local store" sub={['append-only chunk log', 'reclaimed by hole punching']} />

	<Edge points={[[340, 264], [340, 342], [540, 342]]} tone="accent" />
	<Note x={440} y={332} anchor="middle" size={10} tone="accent" text="owner is another host" />
	<Node x={540} y={310} w={230} h={64} title="PUT to owner" sub={['sealed segment; acked after fdatasync', 'extent compacted after the ack']} tone="accent" />
	<Edge points={[[770, 342], [995, 342]]} tone="accent" />
	<Note x={882} y={332} anchor="middle" size={10} tone="accent" text="to the owner on another host" />

	<Node x={760} y={50} w={200} h={52} title="index" sub="hash → store offset, in memory" />
	<Node x={760} y={116} w={200} h={52} title="chunk cache" sub="hash → chunk bytes, bounded" tone="outline" />
	<Node x={760} y={182} w={200} h={52} title="manifests" sub="image offset → chunk hash" />

	<Note x={230} y={396} tone="muted" size={10} text={['read path: staging log, then local store, then chunk cache, then GET from the owner', 'prefetch: the next chunks in the manifest, on sequential reads']} />
</Diagram>

<h2>Write path</h2>
<p>
	Guest writes append at block granularity to a staging log on local NVMe.<br />
	Every append is stamped with a per-image sequence number inside the same critical section as the append, so replay preserves last-write-wins.<br />
	FLUSH is <code>fdatasync</code> of the log, then the acknowledgment, and it covers the highest sequence number seen on any queue of the device, because virtio-blk has no FUA (force unit access) and requests arrive on several queues.<br />
	The hot path hashes nothing and chunks nothing, so large writes proceed at sequential-append speed.
</p>
<p>
	Durability comes from the log alone: the page cache never holds the only copy of anything, and every file is opened O_DIRECT.<br />
	The log is flushed the moment a FLUSH is waiting; there is no linger window, because a linger against a 40 µs fdatasync is slower than the sync.
</p>
<p>
	Staging is finite, so a governor paces compaction on the measured drain rate, with an idle trigger so nothing sits parked after a workload ends.<br />
	When ingest still outruns compaction the guest sees added latency, never a stall, and the log ends in a clean ENOSPC.<br />
	The point where pressure engages, and the latency it adds, are both measured.
</p>

<h2>Compactor</h2>
<p>
	A background pass reads settled extents from staging, cuts them into chunks, hashes each with BLAKE3, and skips any hash that every current owner already holds and has fenced; a copy in a cache does not count.<br />
	Chunking is fixed 4K or FastCDC with boundaries snapped to 4K, chosen per arm on page 02.<br />
	Settled means unwritten for a settle window, so an extent overwritten inside the window is chunked once, in its final form; the window is a parameter and its effect on chunk traffic is measured.
</p>
<p>
	For each new chunk, the owner is the first k hosts in rendezvous order of its hash.<br />
	If the owner is this host, the chunk is appended to the local store and written with fdatasync.<br />
	Otherwise it goes to the owner in a sealed segment of many chunks, which the owner appends, fdatasyncs once, and acks.<br />
	<mark>Only after the ack does the extent count as compacted.</mark><br />
	A chunk the compactor has produced stays pinned, in staging or in the store, until the manifest commit that references it is durable, and an owner never reclaims a chunk it acked before that fence.<br />
	Staging is therefore the write-ahead log for the whole fleet.
</p>
<p>
	Two costs come with this design, and both are measured.<br />
	Every surviving byte is written at least twice, staging then store, plus journal traffic.<br />
	Compaction reads and writes the same device the guest is using, so guest p99 is measured with the compactor active and idle.
</p>
<p class="note">
	CDC over a dirty extent re-chunks from the last settled boundary before it to the first boundary after it that agrees with the existing cut.<br />
	This is the standard resynchronization rule (<a href="https://pdos.csail.mit.edu/papers/lbfs:sosp01/lbfs.pdf" target="_blank" rel="noopener">LBFS</a> locality; <a href="https://huggingface.co/docs/xet/en/chunking" target="_blank" rel="noopener">Xet</a>'s boundary reset), and it is why CDC never runs on the hot path: one aligned write can move every boundary in its neighborhood.
</p>

<h2>Read path</h2>
<p>
	Reads check staging, then the local store, then the chunk cache, then send <code>GET(hash)</code> to the owner.<br />
	The owner answers from its cache if the chunk is hot, otherwise from its store.<br />
	Fresh data is served without indirection; settled data incurs the manifest lookup, the index lookup, and, if the owner is remote, one round trip.<br />
	<code>GET</code> runs on its own connections with priority over <code>PUT</code> and over compaction IO at the serving disk, so a guest-blocking read never waits behind a bulk transfer.
</p>
<p>
	The chunk cache is daemon-owned memory keyed by hash, LRU, with a size that is a parameter.<br />
	Because every file is O_DIRECT, the kernel page cache holds nothing on any host, and the cache size is set equal to ARC on the ZFS configuration.
</p>
<p>
	Prefetch is the daemon issuing the next D hashes from the manifest when it sees sequential reads, and optionally replaying a recorded boot profile.<br />
	D is swept on page 04.
</p>

<h2>Store, index, and manifest</h2>
<p>
	The local store is an append-only log of records (length, hash, checksum, bytes) and is authoritative for the chunks this host owns.<br />
	The index maps hash to offset, lives in memory, and is rebuilt by scanning the store without re-hashing, because the hash is inline; its bytes per TB is the constant the chunk-size arms measure.<br />
	The index is written only after the data it points to is durable, at every fence.<br />
	The manifest, one per image, is a journaled tree from disk offset to chunk hash.<br />
	It lives with the guest's host and moves when the guest does.
</p>

<h2>Protocol</h2>
<div class="table-scroll">
	<table class="spec prose">
		<thead>
			<tr><th>Message</th><th>Reply</th><th>Used by</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">GET(hash)</td><td>bytes</td><td>cold read, prefetch</td></tr>
			<tr><td class="k">PUT(segment)</td><td>ack after one fdatasync</td><td>compactor sending a sealed segment of chunks to an owner</td></tr>
			<tr><td class="k">HAS(hashes)</td><td>bitmap of hashes the owner lacks or has not fenced</td><td>compactor before PUT, so only missing chunks are sent; provisioning verification</td></tr>
			<tr><td class="k">LIVE(epoch, hashes)</td><td>ack</td><td>garbage collection</td></tr>
			<tr><td class="k">JOURNAL(image, range)</td><td>ack after fdatasync</td><td>fleet class: the staging tail to the fixed journal peer on FLUSH</td></tr>
		</tbody>
	</table>
</div>
<p>
	Messages are length-prefixed over kernel TCP with <code>TCP_NODELAY</code>, driven by io_uring.<br />
	<code>GET</code> and <code>JOURNAL</code> have their own connections and priority; <code>PUT</code> is bulk.<br />
	Every message is idempotent and named by hash or sequence number, so any of them can be retried.<br />
	The daemon runs busy-polling or blocking; page 04 measures both, because the scheduler wakeup is part of the cost.<br />
	Rendezvous hashing means a reader already knows the owner of every hash, so nobody looks up anyone else's index.
</p>
<p>
	RDMA and NVMe-oF exports appear on page 04 as probes that show what the kernel stack costs; the architecture does not depend on either.
</p>

<h2>Placement and the parameter k</h2>
<p>
	The owner set of a chunk is the first k hosts in rendezvous order of its hash.<br />
	The journal peer for fleet class is not chosen this way: a journal needs a fixed home with ordered replay, so each image names one peer at creation and keeps it.<br />
	k is the one multi-host parameter.<br />
	With N hosts, k = N places every chunk on every host (replicated) and k = 1 places each chunk on exactly one (partitioned); on the two-host testbed these are k = 2 and k = 1.<br />
	Page 03 measures both, and a deployment would run k ≥ 2 on N ≥ 3 hosts.
</p>

<h2>Durability classes</h2>
<p>
	Durability is a per-image class on one pipeline; the class changes who waits at FLUSH and for how long, and nothing about where bytes end up.<br />
	<strong>Local class</strong>, the default: FLUSH returns after fdatasync of the staging log on this host.<br />
	<strong>Fleet class</strong>: the staging tail since the last FLUSH is sent to the image's journal peer, which appends it to its own log and fdatasyncs; FLUSH returns after both.<br />
	Local class is the contract a local disk gives, which is why it is the default against R0 and R1.<br />
	Fleet class is what every hyperconverged product does before it acknowledges, and page 03 measures what it costs.
</p>
<div class="table-scroll">
	<table class="spec prose">
		<thead>
			<tr><th>Failure</th><th>Local class</th><th>Fleet class</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">daemon crash</td><td>everything: replay the staging log from D, re-run compaction</td><td>same</td></tr>
			<tr><td class="k">host crash, power loss</td><td>everything acked: FLUSH was fdatasync on local NVMe</td><td>same</td></tr>
			<tr><td class="k">host lost</td><td>acknowledged bytes not yet compacted to an owner, exactly (D, E], are lost; R0 and R1 lose everything</td><td>the staging tail survives: the journal peer replays (D, E] onto a new host; chunks the lost host owned survive only if k ≥ 2, as in the row below</td></tr>
			<tr><td class="k">peer lost, k = 1</td><td colspan="2">chunks it owned are unreadable until it returns, and lost if its disk is; a read that needs one waits or fails with an error, never returns stale bytes</td></tr>
		</tbody>
	</table>
</div>
<p>
	Two rules hold in both classes.<br />
	Bytes are durable on local NVMe before they go on the wire, always.<br />
	Transfer is two-phase: the owner fdatasyncs and acks before the sender marks anything compacted or reclaimable.
</p>

<h2>The watermark</h2>
<p>
	Every image carries two integers.<br />
	E is the highest sequence number with no unconfirmed append before it; in local class confirmed means on local NVMe, in fleet class it means on the journal peer too.<br />
	D is the highest sequence number whose chunks are durable at their owners and whose manifest entries are committed.<br />
	FLUSH waits for E. A snapshot cuts at E. The staging log is trimmed below D. Recovery and migration replay exactly (D, E].<br />
	E never skips a hole, because a maximum over confirmations is the answer that loses acknowledged data.
</p>
<p>
	Two logs, staging and the manifest journal, must agree after a crash, and staging is senior.<br />
	Compaction is idempotent, so replaying (D, E] and re-running it produces the same chunks and the same manifest.<br />
	<code>kill -9</code> at any point, then this replay, must pass <code>fio --verify</code> before any number from the daemon is reported.<br />
	Three more cases have tests because each has stalled a guest in a production system: a FLUSH racing writes on another queue, a discard of an unwritten range, and a daemon that stops answering, which leaves the guest in D-state forever because virtio-blk has no timeout.
</p>

<h2>Garbage collection</h2>
<p>
	A chunk is live if any manifest on any host references it, or if an in-flight compaction has pinned it.<br />
	Each host sends its owner the live set for an epoch with <code>LIVE</code>, and the owner sweeps with <code>FALLOC_FL_PUNCH_HOLE</code> over dead records; there are no reference counts.<br />
	ZFS frees an overwritten block the moment its reference count drops; this design does not, so space leaks between sweeps.<br />
	The sweep therefore runs before every capacity measurement, and the bytes it reclaims are reported beside the capacity number as the leak; concurrent collection is out of scope.
</p>

<h2>Out of scope</h2>
<p>
	Membership changes, failure detection, rebalancing when a host joins or leaves, authentication and encryption on the wire, measurement on more than two hosts, and concurrent garbage collection.<br />
	Each is named in future work on page 05, and none of them affects a number this study reports.
</p>

<h2>Provenance</h2>
<div class="table-scroll">
	<table class="spec prose">
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
<p>
	Because the hypervisor is unmodified, no result can be an artifact of a patched QEMU, and the raw-file control runs the identical binary.
</p>

<PageNav num="01" />
