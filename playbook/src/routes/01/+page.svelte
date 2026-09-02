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

<h2>Components on one host</h2>
<p>
	The guest sees a virtio-blk device on stock QEMU.<br />
	QEMU connects it over vhost-user-blk to one process per host, the daemon, and all new code lives in the daemon itself.<br />
	Guest memory is shared with the daemon, so requests are read in place, and storage IO goes through Linux io_uring.<br />
	The device advertises a 4 KiB logical block, so every write is a whole number of blocks and no read-modify-write exists.<br />
	If the daemon restarts, QEMU reconnects over the same vhost-user socket and the requests in flight at the restart are recovered through the vhost-user inflight descriptor mechanism, so a daemon crash is invisible to the guest beyond a pause.
</p>

<h2>Watermark</h2>

<Diagram
	w={960}
	h={236}
	label="One image's sequence numbers on a line, with three marks. Everything at or below D has its chunks in a store and its manifest entry committed, and the staging log is trimmed there. (D, E] is the staging tail, replayed on recovery and migration. FLUSH waits for E and a snapshot cuts at E. O is at or below D and marks what is durable at every owner; (O, E] is what a lost host loses in local class. O equals D unless a surplus copy is standing in for an unreachable owner."
	caption="The watermark of one image, O ≤ D ≤ E. FLUSH waits for E and a snapshot cuts at E. The staging log is trimmed below D. Recovery and migration replay (D, E]. A lost host loses (O, E] in local class."
>
	<Edge points={[[40, 60], [920, 60]]} tone="muted" />
	<Note x={920} y={46} anchor="end" tone="muted" size={10} text="sequence numbers of one image, increasing" />
	<Node x={260} y={44} w={40} h={32} title="O" tone="outline" />
	<Node x={500} y={44} w={40} h={32} title="D" tone="outline" />
	<Node x={740} y={44} w={40} h={32} title="E" tone="outline" />
	<Note x={280} y={98} anchor="middle" size={11} text={['owner-durable', 'every owner has acknowledged']} />
	<Note x={520} y={98} anchor="middle" size={11} text={['compacted', 'in a store, manifest committed']} />
	<Note x={760} y={98} anchor="middle" size={11} text={['acknowledged', 'FLUSH waits here, a snapshot cuts here']} />
	<Group x={40} y={140} w={480} h={28} label="trimmed from the staging log" />
	<Group x={520} y={140} w={240} h={28} label="staging tail, replayed" tone="accent" />
	<Group x={280} y={180} w={480} h={28} label="lost with this host in local class" />
	<Note x={770} y={199} size={10} tone="muted" text="(O, E]" />
	<Note x={770} y={159} size={10} tone="accent" text="(D, E]" />
</Diagram>

<p>
	Every image carries three integers.<br />
	E is the highest sequence number with no unconfirmed append before it. In local class confirmed means on local NVMe. In fleet class it means on the journal peer too.<br />
	D is the highest sequence number whose chunks are durable in a store, at their owners or as surplus copies on this host, and whose manifest entries are committed.<br />
	O ≤ D is the highest sequence number whose chunks are durable at every owner. O equals D except while a surplus copy stands in for an unreachable owner.<br />
	FLUSH waits for E. A snapshot cuts at E. The staging log is trimmed below D, and trimmed regions are discarded so the drive does not copy dead bytes. Recovery and migration replay exactly (D, E]. A lost host loses (O, E] in local class.<br />
	E never skips a hole, because a maximum over confirmations forgets the append still in flight, and that is the answer that loses acknowledged data.
</p>
<p>
	Two logs, staging and the manifest journal, must agree after a crash, and staging is senior.<br />
	Re-running compaction over the replayed extents yields a manifest whose every offset maps to the same bytes. It need not yield the same chunk boundaries under CDC, and the sweep reclaims the orphans of the first run.<br />
	<code>kill -9</code> at any point, then this replay, must pass <code>fio --verify</code> before any number from the daemon is reported. The log's torn tail is tested in both shapes, a shortened file and a partial record followed by preallocated zeros.<br />
	Three more cases have tests because each is a defect the author met in a prior implementation:
</p>
<ul class="plain">
	<li>an empty discard that acknowledged a sequence number nothing wrote and wedged the next FLUSH</li>
	<li>a FLUSH that must cover writes completed on any queue, checked with a multi-queue test and a negative control that shows the test can see the reordering</li>
	<li>a daemon that stops answering, which leaves the guest in D-state because virtio-blk installs no timeout handler</li>
</ul>

<h2>Write path</h2>

<Diagram
	w={960}
	h={300}
	label="The write path on one host. The guest's virtio-blk writes reach the daemon over vhost-user and append to a per-image staging log on local NVMe, opened O_DIRECT, with a sequence number per append. FLUSH is an fdatasync of the log followed by the acknowledgment. In fleet class a JOURNAL message carries the staging tail to the journal peer on another host, in parallel with the local fdatasync, and the acknowledgment waits for both. Nothing on this path hashes or chunks."
	caption="The write path. A write is one append to a per-image log. FLUSH is one fdatasync, and in fleet class one round trip beside it."
>
	<Node x={20} y={40} w={140} h={52} title="guest" sub={['virtio-blk', '4 KiB logical blocks']} tone="muted" />
	<Node x={20} y={124} w={140} h={44} title="stock QEMU" sub="vhost-user-blk" tone="muted" />
	<Edge points={[[160, 146], [210, 146]]} label="shared memory" labelDy={-8} />

	<Group x={210} y={20} w={460} h={262} label="daemon on host A" tone="accent" />
	<Node x={236} y={56} w={250} h={64} title="staging log, one per image" sub={['append at block granularity', 'sequence number per append']} tone="accent" />
	<Edge points={[[361, 120], [361, 190]]} label="O_DIRECT write" labelDx={58} />
	<Node x={236} y={190} w={250} h={44} title="local NVMe" tone="muted" />
	<Edge points={[[486, 88], [520, 88]]} />
	<Node x={520} y={56} w={130} h={64} title="FLUSH" sub={['fdatasync the log', 'then ack the guest']} tone="outline" />
	<Note x={236} y={262} tone="muted" size={10} text="no hashing and no chunking on this path" />

	<Edge points={[[650, 88], [720, 88]]} tone="accent" dashed label="fleet class only" labelDy={-8} />
	<Node x={720} y={56} w={220} h={64} title="journal peer, host B" sub={['JOURNAL(image, range)', 'append, fdatasync, ack']} tone="ghost" />
	<Note x={720} y={150} tone="accent" size={10} text={['local class: ack after the local fdatasync', 'fleet class: ack after both,', 'the send in parallel with the fdatasync']} />
</Diagram>

<p>
	Guest writes append at block granularity to a staging log on local NVMe, one log per image.<br />
	Every append is stamped with a per-image sequence number inside the same critical section as the append, so log order is sequence order and replay preserves last-write-wins.<br />
	FLUSH is <code>fdatasync</code> of the log followed by the guest ack. It covers the highest sequence number completed on any queue of the device, because virtio-blk has no FUA (force unit access) and requests arrive via several queues.<br />
	If the guest negotiates neither VIRTIO_BLK_F_FLUSH nor a writeback cache, every write is acknowledged after fdatasync.<br />
	The hot path hashes nothing and chunks nothing, so we predict a large write proceeds at the sequential-append speed of the NVMe.
</p>
<p>
	Durability comes from the log alone. Every file is opened O_DIRECT, so the page cache must never hold the sole copy of a byte.<br />
	A waiting FLUSH starts an fdatasync at once. FLUSHes that arrive during one are covered by the next, and an idle fdatasync every 50 ms upgrades writes no FLUSH has asked for.<br />
	An in-memory map from block to log offset locates fresh data in the log.
</p>
<p>
	The staging log is finite, so a governor paces compaction on the measured drain rate, with an idle trigger so that nothing sits in staging after a workload ends.<br />
	When ingest outruns compaction the guest sees added latency and never an error, because virtio-blk has no out-of-space status and an IO error shuts down the guest's filesystem.<br />
	The point where pressure engages, and the latency it adds, are both measured in this study.
</p>

<h2>Compactor</h2>

<Diagram
	w={960}
	h={390}
	label="The compactor. Settled extents leave the staging log, are cut into chunks (fixed 4 KiB, fixed 16 KiB, or FastCDC snapped to 4 KiB), and are hashed with BLAKE3. Rendezvous order of the hash names the owner set, and HAS asks each owner what it lacks. A chunk this host owns is appended to the local store and fdatasynced. A chunk another host owns goes there in a sealed segment, which the owner appends, fdatasyncs, and acknowledges. If an owner is unreachable, the chunk is kept in the local store as a pinned surplus copy and a repair queue retries the send. An extent counts as compacted when its chunks are durable in a store and the manifest commit that references it is durable; the staging log is trimmed below D, and O trails D while a surplus copy stands in for an owner."
	caption="The compactor. It is the only component that sends bytes to another host on the write side, and it does so after the guest's acknowledgment."
>
	<Node x={20} y={40} w={190} h={56} title="staging log" sub="settled extents" tone="muted" />
	<Edge points={[[210, 68], [250, 68]]} />
	<Node x={250} y={40} w={180} h={56} title="chunk" sub="fixed 4 or 16 KiB, or FastCDC" tone="outline" />
	<Edge points={[[430, 68], [470, 68]]} />
	<Node x={470} y={40} w={150} h={56} title="BLAKE3" sub="one hash per chunk" tone="outline" />
	<Edge points={[[620, 68], [660, 68]]} />
	<Node x={660} y={40} w={280} h={56} title="owners = rendezvous(hash)" sub="first k hosts; HAS asks what they lack" tone="accent" />

	<Edge points={[[800, 96], [800, 130], [150, 130], [150, 170]]} />
	<Edge points={[[800, 130], [480, 130], [480, 170]]} />
	<Edge points={[[800, 96], [800, 170]]} />
	<Node x={40} y={170} w={220} h={64} title="this host owns it" sub={['append to the local store', 'fdatasync']} />
	<Node x={370} y={170} w={220} h={64} title="another host owns it" sub={['PUT a sealed segment', 'owner appends, fdatasyncs, acks']} tone="accent" />
	<Node x={680} y={170} w={240} h={64} title="owner unreachable" sub={['surplus copy in the local store, pinned', 'a repair queue retries the PUT']} tone="ghost" />

	<Edge points={[[150, 234], [150, 270], [480, 270], [480, 300]]} />
	<Edge points={[[480, 234], [480, 300]]} />
	<Edge points={[[800, 234], [800, 270], [480, 270]]} arrow={false} />
	<Node x={250} y={300} w={460} h={64} title="extent compacted" sub={["after every owner's ack and the manifest commit", 'staging trimmed below D; the chunk pinned until then']} tone="accent" />
</Diagram>

<p>
	A background pass reads settled extents from the staging log, cuts them into chunks, hashes each with BLAKE3, and skips any hash that every current owner already holds and has fenced.<br />
	A copy in a cache does not count as held.<br />
	Chunking is fixed 4 KiB, fixed 16 KiB, or FastCDC with boundaries snapped to 4 KiB (one per measurement arm on page 02).<br />
	Settled means unwritten for a settle window, so an extent overwritten inside the window is chunked once, after the window closes, in its final form.<br />
	The window is a parameter, and its effect on chunk traffic is measured.<br />
	A discarded or zero-filled range is one range entry in the manifest that names no chunk, so DISCARD and WRITE_ZEROES of any size consume no store space and constant compactor work. A read of such a range returns zeros.
</p>
<p>
	Rendezvous order of the hash names the k owners (Placement, below).<br />
	If this host is an owner, the chunk is appended to the local store and made durable with fdatasync.<br />
	Otherwise it goes to each owner in a sealed segment of many chunks, which the owner appends, fdatasyncs once, and acknowledges.<br />
	<mark>An extent counts as compacted at D, when its chunks are durable in a store, at their owners or as surplus copies here, and its manifest entry is committed. It counts as owner-durable at O, after every owner's acknowledgment.</mark><br />
	If an owner is unreachable, the compactor appends the chunk to the local store as a surplus copy, pinned until that owner acknowledges it later. A repair queue retries the send.<br />
	The staging log therefore trims on local durability alone. A peer outage costs a writer latency and nothing else; what it costs a reader is the k = 1 row of the failure table below. The sweep reclaims the surplus after the acknowledgment.<br />
	A chunk the compactor has produced stays pinned, in the staging log or in a store, until the manifest commit that references it is durable. An owner never reclaims a chunk it acknowledged before that fence.<br />
	The staging log is therefore the write-ahead log for every chunk this host produces, wherever the chunk might end up.
</p>
<p>
	The compactor holds no lock the FLUSH path takes. A test slows the store to one second per append and checks that every FLUSH still completes within its budget.
</p>
<p class="note">
	CDC over a dirty extent re-chunks from the last settled boundary before it to the first boundary after it that agrees with the existing cut.<br />
	Two published properties make the rule exact (<a href="https://pdos.csail.mit.edu/papers/lbfs:sosp01/lbfs.pdf" target="_blank" rel="noopener">LBFS</a> locality; <a href="https://huggingface.co/docs/xet/en/chunking" target="_blank" rel="noopener">Xet</a>'s boundary reset), and it is why CDC never runs on the hot path. One aligned write can move every boundary in its neighborhood.
</p>

<h2>Read path</h2>

<Diagram
	w={960}
	h={300}
	label="The read path. A read checks the staging log, then the chunk cache, then the local store, and otherwise sends GET for the hash to the first reachable owner in rendezvous order. The owner answers from its cache or its store. A reply is hashed before it is served. On sequential reads the daemon asks for the next P hashes in one GET."
	caption="The read path. Three local checks, then one round trip. The network appears only in the last box."
>
	<Node x={20} y={60} w={200} h={40} title="in the staging log?" kind="question" />
	<Edge points={[[220, 80], [250, 80]]} label="no" labelDy={-6} />
	<Node x={250} y={60} w={200} h={40} title="in the chunk cache?" kind="question" />
	<Edge points={[[450, 80], [480, 80]]} label="no" labelDy={-6} />
	<Node x={480} y={60} w={200} h={40} title="in the local store?" kind="question" />
	<Edge points={[[680, 80], [700, 80]]} label="no" labelDy={-6} tone="accent" />
	<Node x={700} y={50} w={250} h={60} title="GET(hash) to the first reachable owner" sub="answered from its cache, else its store" tone="accent" />

	<Edge points={[[120, 100], [120, 170]]} label="yes" labelDx={14} />
	<Edge points={[[350, 100], [350, 170]]} label="yes" labelDx={14} />
	<Edge points={[[580, 100], [580, 170]]} label="yes" labelDx={14} />
	<Edge points={[[825, 110], [825, 170]]} tone="accent" />
	<Node x={20} y={170} w={200} h={52} title="one NVMe read" sub="fresh data, no indirection" />
	<Node x={250} y={170} w={200} h={52} title="memory hit" sub="no IO" />
	<Node x={480} y={170} w={200} h={52} title="one NVMe read" sub="index lookup first" />
	<Node x={700} y={170} w={250} h={52} title="one round trip" sub="reply hashed before it is served" tone="accent" />

	<Note x={20} y={262} tone="muted" size={10} text="prefetch: on sequential reads the daemon asks for the next P hashes in one GET; the guest's own readahead adds to P" />
</Diagram>

<p>
	A read checks the staging log, then the chunk cache, then the local store if this host holds the chunk. Otherwise it sends <code>GET</code> for the hash to the first reachable owner in rendezvous order.<br />
	The owner answers from its cache if the chunk is hot and from its store otherwise.<br />
	Every chunk that arrives over the network is hashed before it is used, so a wrong or corrupt reply is detected and never served. A record read from a store is checked against its inline checksum.<br />
	Fresh data is served without indirection. Settled data incurs the manifest lookup, the index lookup, and, if the chunk is remote, one round trip.<br />
	<code>GET</code> runs on its own connections with priority over <code>PUT</code> and over compaction IO at the serving disk, so a guest-blocking read never waits behind a bulk transfer.
</p>
<p>
	The chunk cache is daemon-owned memory keyed by hash, LRU, with a size that is a parameter.<br />
	A fetched chunk this host does not own lives in that memory cache only.<br />
	A disk tier for fetched chunks, as Liquid had, is a knob measured only if time remains, since page 04 predicts a refetch from a peer's memory costs less than a local disk hit.
</p>
<p>
	Prefetch is the daemon issuing the next P hashes from the manifest in one <code>GET</code> when it sees sequential reads, and optionally replaying a recorded boot profile.<br />
	The guest's own readahead is left at its default and adds to P.<br />
	P is swept on page 04.
</p>

<h2>Store, index, and manifest</h2>
<p>
	The local store is an append-only log of records (length, hash, checksum, bytes) and is authoritative for the chunks this host owns.<br />
	The index maps hash to store offset, lives in memory, and is rebuilt by scanning the store without re-hashing, because the hash is inline.<br />
	Its bytes per TB is the constant the chunk-size arms measure.<br />
	In partitioned mode a host indexes the chunks it owns plus any surplus copies awaiting an owner, so per-host index memory is k/N of the fleet's once the repair queue is empty.<br />
	An index entry is added only after the data it points to is durable, at every fence.<br />
	The manifest, one per image, is a journaled tree from disk offset to chunk hash, packed in offset order.<br />
	It lives with the guest's host and moves when the guest does.
</p>

<h2>Protocol</h2>
<div class="table-scroll">
	<table class="spec prose">
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
	<table class="spec prose">
		<thead>
			<tr><th>Failure</th><th>Local class</th><th>Fleet class</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">daemon crash</td><td>nothing acknowledged is lost: replay the staging log from D, re-run compaction</td><td>same</td></tr>
			<tr><td class="k">host crash, power loss</td><td>nothing acknowledged is lost: FLUSH was fdatasync on local NVMe</td><td>same</td></tr>
			<tr><td class="k">host lost</td><td>acknowledged bytes not yet durable at an owner, exactly (O, E], are lost; R0 and R1 lose everything</td><td>the staging tail survives: the journal peer replays (D, E] onto a new host; chunks the lost host owned survive only if k ≥ 2, as in the row below</td></tr>
			<tr><td class="k">peer lost, k = 1</td><td>chunks it owned are unreadable until it returns, and lost if its disk is; a read that needs one waits or fails with an error, never returns stale bytes; writes continue, with surplus copies standing in</td><td>the same for reads; a FLUSH cannot reach the journal peer, so the image drops to local class for the interval, which is logged and reported, and returns to fleet class when the peer does</td></tr>
		</tbody>
	</table>
</div>
<p>
	Two rules hold in both classes:
</p>
<ul class="plain">
	<li>The compactor never sends a chunk whose staging extent is not yet durable on this host.</li>
	<li>Transfer is a two-phase job. The owner fdatasyncs and acknowledges before the sender marks anything compacted or reclaimable.</li>
</ul>

<h2>Garbage collection (GC)</h2>
<p>
	A chunk is live if any manifest on any host references it, or if an in-flight compaction has pinned it. A copy in a cache is never a reference.<br />
	Each host sends each owner the live set for an epoch with <code>LIVE</code>, and the owner sweeps with <code>FALLOC_FL_PUNCH_HOLE</code> over dead records.<br />
	Refcounting is not a concept in this architecture.<br />
	ZFS frees an overwritten block when its reference count drops. This design does not, so space can leak between sweeps.<br />
	The sweep therefore runs before every capacity measurement, and the bytes it reclaims are reported beside the capacity number as the leak.
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

<PageNav num="01" />
