<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
	import { Diagram, Node, Edge, Group, Note } from '$lib/components/diagram';
</script>

<PageHead num="01" />
<p class="lede">
	In local class the network is on the read path only, and only for a chunk this host does not hold.<br />
	In fleet class it is also on the FLUSH path, once per FLUSH, to one fixed peer.<br />
	No other message precedes an acknowledgment.
</p>

<h2>Components on one host</h2>
<p>
	The guest sees a virtio-blk device on stock QEMU.<br />
	QEMU connects it over vhost-user-blk to one process per host, the daemon, and all new code lives there.<br />
	Guest memory is shared with the daemon, so requests are read in place, and storage IO goes through io_uring.<br />
	The device advertises a 4 KiB logical block, so every write is a whole number of blocks and no read-modify-write exists.
</p>

<Diagram
	w={1000}
	h={440}
	label="The per-host datapath. A guest on stock QEMU reaches the daemon over vhost-user. Writes append to a local staging log and are acknowledged at FLUSH after fdatasync. A background compactor chunks settled extents, hashes them, and either appends unique chunks to the local store or sends them to their owner on another host, waiting for a durable ack. Reads check staging, then the chunk cache, then the local store, then fetch by hash from the owner."
	caption="One host. The write path ends at the staging log. The compactor is the only component that talks to other hosts on the write side, and it does so after the acknowledgment."
>
	<Node x={20} y={44} w={130} h={52} title="guest" sub="virtio-blk" tone="muted" />
	<Node x={20} y={116} w={130} h={44} title="stock QEMU" sub="vhost-user-blk" tone="muted" />
	<Edge points={[[150, 138], [200, 138]]} />
	<Note x={22} y={110} size={9.5} tone="muted" text="shared guest memory" />

	<Group x={200} y={20} w={780} h={400} label="daemon · one per host" tone="accent" />

	<Node x={230} y={60} w={220} h={64} title="staging log" sub={['append-only · local NVMe', 'FLUSH → fdatasync → ack (local class)']} tone="accent" />
	<Note x={470} y={86} tone="muted" size={9.5} text={['no hashing or chunking on this path', 'fleet class adds one JOURNAL round trip to a fixed peer, in parallel with the fdatasync']} />

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

	<Note x={230} y={396} tone="muted" size={10} text={['read path: staging log, then chunk cache, then local store, then GET from the owner', 'prefetch: the next chunks in the manifest, on sequential reads']} />
</Diagram>

<h2>Write path</h2>
<p>
	Guest writes append at block granularity to a staging log on local NVMe.<br />
	Every append is stamped with a per-image sequence number inside the same critical section as the append, so log order is sequence order and replay preserves last-write-wins.<br />
	FLUSH is <code>fdatasync</code> of the log followed by the acknowledgment, and it covers the highest sequence number completed on any queue of the device, because virtio-blk has no FUA (force unit access) and requests arrive on several queues.<br />
	If the guest negotiates neither VIRTIO_BLK_F_FLUSH nor a writeback cache, every write is acknowledged after fdatasync.<br />
	The hot path hashes nothing and chunks nothing, so a large write proceeds at sequential-append speed.
</p>
<p>
	Durability comes from the log alone: every file is opened O_DIRECT, so the page cache never holds the only copy of anything.<br />
	A waiting FLUSH starts an fdatasync at once, FLUSHes that arrive during one are covered by the next, and an idle fdatasync every 50 ms upgrades writes no FLUSH has asked for.<br />
	There is no timed linger: in a prior implementation by the author, a 500 µs linger in front of an fdatasync averaging 35 µs was unproductive 97% of the time (unpublished measurement).<br />
	Fresh data is read back from the log at the cost of one NVMe read, the cost R0 pays; an in-memory map from block to log offset supplies the location.
</p>
<p>
	The staging log is finite, so a governor paces compaction on the measured drain rate, with an idle trigger so that nothing sits in staging after a workload ends.<br />
	When ingest outruns compaction the guest sees added latency and never an error, because virtio-blk has no out-of-space status and an IO error shuts down the guest's filesystem.<br />
	The point where pressure engages, and the latency it adds, are both measured.
</p>

<h2>Compactor</h2>
<p>
	A background pass reads settled extents from the staging log, cuts them into chunks, hashes each with BLAKE3, and skips any hash that every current owner already holds and has fenced; a copy in a cache does not count.<br />
	Chunking is fixed 4 KiB, fixed 16 KiB, or FastCDC with boundaries snapped to 4 KiB, chosen per arm on page 02.<br />
	Settled means unwritten for a settle window, so an extent overwritten inside the window is chunked once, in its final form; the window is a parameter and its effect on chunk traffic is measured.<br />
	A discarded or zero-filled range is one range entry in the manifest that names no chunk, so DISCARD and WRITE_ZEROES of any size consume no store space and constant compactor work.<br />
	Deferred hashing behind a write buffer is <a href="https://madsys.cs.tsinghua.edu.cn/publications/TPDS2014-zhao.pdf" target="_blank" rel="noopener">Liquid</a>'s design and <a href="https://github.com/wangeguo/plan9/blob/master/sys/man/4/fossil" target="_blank" rel="noopener">Fossil</a>'s before it; the buffer here is a durable log with a FLUSH contract rather than memory flushed at shutdown.
</p>
<p>
	For each new chunk, the owner set is the first k hosts in rendezvous order of its hash.<br />
	If this host is an owner, the chunk is appended to the local store and made durable with fdatasync.<br />
	Otherwise it goes to each owner in a sealed segment of many chunks, which the owner appends, fdatasyncs once, and acknowledges.<br />
	<mark>Only after every owner's acknowledgment does the extent count as compacted.</mark><br />
	If an owner is unreachable, the compactor appends the chunk to the local store as a surplus copy, pinned until that owner acknowledges it later; the staging log therefore trims on local durability alone, a peer outage costs the guest latency and nothing else, and the sweep reclaims the surplus after the acknowledgment.<br />
	A chunk the compactor has produced stays pinned, in the staging log or in a store, until the manifest commit that references it is durable, and an owner never reclaims a chunk it acknowledged before that fence.<br />
	The staging log is therefore the write-ahead log for every chunk this host produces, wherever the chunk ends up.
</p>
<p>
	Two costs come with this design, and both are measured.<br />
	Every surviving byte is written at least twice, staging then store, plus journal traffic.<br />
	Compaction reads and writes the same device the guest is using, so guest p99 is measured with the compactor active and idle.<br />
	The compactor holds no lock the FLUSH path takes, and a test slows the store to one second per append and checks that every FLUSH still completes within its budget.
</p>
<p class="note">
	CDC over a dirty extent re-chunks from the last settled boundary before it to the first boundary after it that agrees with the existing cut.<br />
	This is the standard resynchronization rule (<a href="https://pdos.csail.mit.edu/papers/lbfs:sosp01/lbfs.pdf" target="_blank" rel="noopener">LBFS</a> locality; <a href="https://huggingface.co/docs/xet/en/chunking" target="_blank" rel="noopener">Xet</a>'s boundary reset), and it is why CDC never runs on the hot path: one aligned write can move every boundary in its neighborhood.
</p>

<h2>Read path</h2>
<p>
	A read checks the staging log, then the chunk cache, then the local store if this host holds the chunk, and otherwise sends <code>GET</code> for the hash to an owner.<br />
	The owner answers from its cache if the chunk is hot and from its store otherwise.<br />
	Every chunk that arrives over the network is hashed before it is used, so a wrong or corrupt reply is detected and never served.<br />
	Fresh data is served without indirection; settled data incurs the manifest lookup, the index lookup, and, if the chunk is remote, one round trip.<br />
	<code>GET</code> runs on its own connections with priority over <code>PUT</code> and over compaction IO at the serving disk, so a guest-blocking read never waits behind a bulk transfer.
</p>
<p>
	The chunk cache is daemon-owned memory keyed by hash, LRU, with a size that is a parameter.<br />
	A fetched chunk this host does not own lives in that memory cache only.<br />
	Liquid persisted fetched blocks in an on-disk copy-on-read cache; here a refetch from a peer's memory is predicted at 20 to 30 µs against about 80 µs for a local disk hit, so a disk tier would pay only for chunks that are cold at their owner too.<br />
	The disk tier is a knob, noted, and measured only if time remains; the residual cost of one copy per chunk on page 04 is measured without it.<br />
	Because every file is O_DIRECT, the kernel page cache holds nothing on any host, and the cache size is set equal to the ARC (adaptive replacement cache) limit on the ZFS configuration.
</p>
<p>
	Prefetch is the daemon issuing the next D hashes from the manifest in one <code>GET</code> when it sees sequential reads, and optionally replaying a recorded boot profile.<br />
	The guest's own readahead is left at its default and adds to D.<br />
	D is swept on page 04.
</p>

<h2>Store, index, and manifest</h2>
<p>
	The local store is an append-only log of records (length, hash, checksum, bytes) and is authoritative for the chunks this host owns.<br />
	The index maps hash to store offset, lives in memory, and is rebuilt by scanning the store without re-hashing, because the hash is inline; its bytes per TB is the constant the chunk-size arms measure.<br />
	In partitioned mode a host indexes only the chunks it owns, so per-host index memory is k/N of the fleet's.<br />
	The index is written only after the data it points to is durable, at every fence.<br />
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
			<tr><td class="k">JOURNAL(image, range)</td><td>ack after fdatasync</td><td>fleet class: the staging tail to the fixed journal peer on FLUSH</td></tr>
		</tbody>
	</table>
</div>
<p>
	Messages are length-prefixed over kernel TCP with <code>TCP_NODELAY</code>, driven by io_uring.<br />
	<code>GET</code> and <code>JOURNAL</code> have their own connections and priority; <code>PUT</code> is bulk.<br />
	Every message is idempotent and named by hash or sequence number, so any of them can be retried.<br />
	The daemon runs busy-polling or blocking; page 04 measures both, because the scheduler wakeup is part of the cost.<br />
	Rendezvous hashing means a reader already knows the owners of every hash, so no host looks up another's index.
</p>
<p>
	RDMA and NVMe-oF exports appear on page 04 as probes that show what the kernel stack costs; the architecture depends on neither.
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
	<strong>Fleet class</strong>: the staging tail since the last FLUSH is sent to the image's journal peer, which appends it to its own log and fdatasyncs; the send proceeds in parallel with this host's fdatasync, FLUSH returns after both, and FLUSHes from several images to the same peer share one round trip and one fdatasync.<br />
	Local class is the contract a local disk gives, which is why it is the default against R0 and R1.<br />
	Fleet class is what <a href="https://www.nutanixbible.com/4g-book-of-aos-data-io-path.html" target="_blank" rel="noopener">Nutanix AOS</a> and <a href="https://experistg.com/wp-content/uploads/2019/12/The-technology-enabling-HPE-SimpliVity-data-efficiency.pdf" target="_blank" rel="noopener">HPE SimpliVity</a> do before they acknowledge, and page 03 measures what it costs.
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
			<tr><td class="k">peer lost, k = 1</td><td colspan="2">chunks it owned are unreadable until it returns, and lost if its disk is; a read that needs one waits or fails with an error, never returns stale bytes; writes continue, with surplus copies standing in</td></tr>
		</tbody>
	</table>
</div>
<p>
	Two rules hold in both classes.<br />
	The compactor never sends a chunk whose staging extent is not yet durable on this host.<br />
	Transfer is two-phase: the owner fdatasyncs and acknowledges before the sender marks anything compacted or reclaimable.
</p>

<h2>The watermark</h2>
<p>
	Every image carries three integers.<br />
	E is the highest sequence number with no unconfirmed append before it; in local class confirmed means on local NVMe, in fleet class it means on the journal peer too.<br />
	D is the highest sequence number whose chunks are durable in a store, at their owners or as surplus copies on this host, and whose manifest entries are committed.<br />
	O ≤ D is the highest sequence number whose chunks are durable at every owner; O equals D except while a surplus copy stands in for an unreachable owner.<br />
	FLUSH waits for E. A snapshot cuts at E. The staging log is trimmed below D, and trimmed regions are discarded so the drive does not copy dead bytes. Recovery and migration replay exactly (D, E]. A lost host loses (O, E] in local class.<br />
	E never skips a hole, because a maximum over confirmations forgets the append still in flight, and that is the answer that loses acknowledged data.
</p>
<p>
	Two logs, staging and the manifest journal, must agree after a crash, and staging is senior.<br />
	Re-running compaction over the replayed extents yields a manifest whose every offset maps to the same bytes; it need not yield the same chunk boundaries under CDC, and the sweep reclaims the orphans of the first run.<br />
	<code>kill -9</code> at any point, then this replay, must pass <code>fio --verify</code> before any number from the daemon is reported; the log's torn tail is tested in both shapes, a shortened file and a partial record followed by preallocated zeros.<br />
	Three more cases have tests because each is a defect the author met in a prior implementation: an empty discard that acknowledged a sequence number nothing wrote and wedged the next FLUSH; a FLUSH that must cover writes completed on any queue, checked with a multi-queue test and a negative control that shows the test can see the reordering; and a daemon that stops answering, which leaves the guest in D-state because virtio-blk installs no timeout handler.
</p>

<h2>Garbage collection</h2>
<p>
	A chunk is live if any manifest on any host references it, or if an in-flight compaction has pinned it; a copy in a cache is never a reference.<br />
	Each host sends each owner the live set for an epoch with <code>LIVE</code>, and the owner sweeps with <code>FALLOC_FL_PUNCH_HOLE</code> over dead records; there are no reference counts.<br />
	Liquid ran the same mark-and-sweep with Bloom-filter live sets over its data servers.<br />
	ZFS frees an overwritten block the moment its reference count drops; this design does not, so space leaks between sweeps.<br />
	The sweep therefore runs before every capacity measurement, and the bytes it reclaims are reported beside the capacity number as the leak; concurrent collection is out of scope.
</p>

<h2>Out of scope</h2>
<p>
	Membership changes, failure detection, rebalancing when a host joins or leaves, authentication and encryption on the wire, measurement on more than two hosts, and concurrent garbage collection.<br />
	Each is named in future work on page 05, and none affects a number this study reports.
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
