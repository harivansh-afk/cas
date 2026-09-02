<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
	import { Diagram, Node, Edge, Group, Note } from '$lib/components/diagram';
</script>

<PageHead num="00" />
<p class="lede">
	A deduplication table shares duplicate data within one host.<br />
	Content addressing shares it across hosts.<br />
	On Linux VM fleets the first is already solved by ZFS.<br />
	So the case for content addressing rests on what a hash does that a block address cannot: move only unique bytes between hosts, store each chunk k times across a fleet instead of once per host, and serve a chunk from whichever host holds it in memory.
</p>
<p>
	This study builds that system on a stock hypervisor and measures what each of those provides and what each costs.
</p>

<h2>Current solutions: single-host, cross-VM deduplication</h2>
<p>
	Consider two VMs that each run <code>apt upgrade</code> and download the same packages. Their disks now hold the same bytes. Under copy-on-write alone no clone can share them, because neither copy descends from the other.
</p>
<p>
	ZFS solves this with a deduplication table (the DDT), and the Linux kernel has dm-vdo, which does block-level deduplication, compression, and thin provisioning under any filesystem.<br />
	Both hash every block and share equal ones at a fixed, aligned block size: 4K for dm-vdo, and the volblocksize for a ZFS zvol, 16K by default.
</p>
<p>
	Nearly everything inside a Linux guest is 4K aligned (ext4 uses 4K blocks, partitions start at 1 MiB, and package managers write whole files), so at 4K a deduplication table reaches nearly all of the duplicate data.<br />
	This is why <a href="https://ssrc.us/media/pubs/082a25b906aa716ca3c2439b8c1889449ecac44c.pdf" target="_blank" rel="noopener">Jin and Miller</a> found in 2009 that fixed blocks match content-defined chunking on VM images.
</p>
<p>
	<mark>Therefore, on a single host, content addressing has zero capacity win over ZFS at a 4K chunk size.</mark><br />
	Part 1 of this study measures our CAS system against ZFS and tests this assertion.
</p>
<p>
	None of this works across hosts.<br />
	The DDT is per pool, dm-vdo has no replication, and <code>zfs send</code> has not carried a deduplicated stream since OpenZFS 2.0.<br />
	With a fleet of N hosts each running its own deduplication store, every unique chunk is stored N times.<br />
	As a side effect, moving a VM to another host means sending every one of its chunks over the wire (via <code>zfs send</code> or similar), whether or not the destination already holds them.
</p>
<p>
	Shared-storage systems (Ceph RBD with TiDedup, for example) do deduplicate across hosts, by putting every write on the network before it is acknowledged.<br />
	That is the other end of the design space, and page 06 places this study against it.
</p>

<Diagram
	w={960}
	h={284}
	label="Left: two hosts each with their own deduplication table and no link between them; the same chunk is stored on both and moves whole when a guest migrates. Right: two hosts whose chunks are named by hash and owned by hash across both; a chunk is stored k times fleet-wide, a guest's manifest moves while its chunks stay, and a cold read fetches by hash from the owner."
	caption="Left: a deduplication table shares within a host. Right: a hash is the same on every host, so placement, transfer, and cache follow the content."
>
	<Group x={20} y={20} w={440} h={244} label="per-host deduplication" />
	<Node x={40} y={56} w={195} h={44} title="host A" sub="guests on ZFS" tone="muted" />
	<Node x={40} y={116} w={195} h={44} title="DDT A" sub="hash → block address in pool A" />
	<Node x={245} y={56} w={195} h={44} title="host B" sub="guests on ZFS" tone="muted" />
	<Node x={245} y={116} w={195} h={44} title="DDT B" sub="hash → block address in pool B" />
	<Note x={240} y={190} anchor="middle" tone="muted" text={['the two tables are independent', 'a shared chunk is stored on both hosts', 'migration copies the whole image']} />

	<Group x={500} y={20} w={440} h={244} label="content addressing" tone="accent" />
	<Node x={520} y={56} w={180} h={44} title="host A" sub="guests on the daemon" tone="muted" />
	<Node x={520} y={116} w={180} h={44} title="chunks owned by A" sub="owner chosen by hash" tone="accent" />
	<Node x={740} y={56} w={180} h={44} title="host B" sub="guests on the daemon" tone="muted" />
	<Node x={740} y={116} w={180} h={44} title="chunks owned by B" sub="owner chosen by hash" tone="accent" />
	<Edge points={[[700, 130], [740, 130]]} tone="accent" />
	<Edge points={[[740, 146], [700, 146]]} tone="accent" />
	<Note x={720} y={110} anchor="middle" tone="accent" size={10} text="GET and PUT by hash" />
	<Note x={720} y={190} anchor="middle" tone="accent" text={['one chunk namespace across hosts', 'a shared chunk is stored k times fleet-wide', 'migration copies the manifest; the chunks stay', 'a cold read fetches the chunk from its owner']} />
</Diagram>

<h2>Advantages of content-addressing your data</h2>
<p>
	A chunk named by its hash has the same name on every host, so placement is a function of the content itself.<br />
	Three things follow, and each is a measured claim in parts 2 and 3.
</p>
<p>
	<strong>Transfer.</strong><br />
	Provisioning a guest moves only its manifest (32 bytes per chunk) and no chunk data, because every chunk it names already exists at its owner.<br />
	Migrating a guest moves the manifest plus whatever it wrote since the last compaction.<br />
	Migration is not the point of the study; it is the operation where "only unique bytes cross the wire" is easiest to see and to measure.
</p>
<p>
	<strong>Capacity.</strong><br />
	A fleet stores each chunk k times, not once per host.
</p>
<p>
	<strong>Cache.</strong><br />
	A chunk that is hot anywhere is in some host's memory, and <mark>a peer's memory over 100 GbE is closer than local NVMe</mark>: about 20 µs against about 80.
</p>

<h2>The cost</h2>
<p>
	Three costs come with any deduplicating store, and we measure them rather than assume them: write amplification (every byte is written to the staging log and again to the chunk store), compactor interference (compaction shares the guest's disk), and index memory (one entry per chunk, in RAM).
</p>
<p>
	One cost is specific to crossing hosts: the network sits on the read path for cold chunks.<br />
	A guest read whose chunk lives on another host pays one round trip.<br />
	Part 3 measures that round trip over TCP and over RDMA (remote direct memory access), from the peer's memory and from the peer's NVMe.<br />
	It then measures prefetch: the daemon knows from the manifest which chunks come next, so it fetches them before the guest asks, and the round trip overlaps with work the guest is already doing instead of adding to it.<br />
	How much of the cost prefetch removes is the number.
</p>
<p>
	One cost is a trade rather than a tax: durability before acknowledgment.<br />
	A guest's FLUSH means "these bytes must survive".<br />
	In local class, the default, the daemon acknowledges after fdatasync on this host, which is the same contract a local disk gives; if the host is lost before compaction has shipped those bytes to their owner, they are lost with it.<br />
	In fleet class the daemon first sends the bytes themselves (not the manifest, since the manifest points at bytes that exist nowhere else yet) to a fixed peer, waits for the peer's fdatasync, and then acknowledges; the bytes now survive the loss of this host.<br />
	Every hyperconverged product works in fleet class.<br />
	Parts 2 and 3 measure the price of the difference: one round trip and one remote fdatasync per FLUSH, on TCP, and on RDMA if that arm lands.
</p>

<h2>Hypotheses</h2>
<p>
	<strong>1. Single-host parity.</strong><br />
	Our CAS system stores within 10% of the bytes ZFS fast dedup stores at the same block size, with guest p99 within 20% of a raw file on XFS.<br />
	Index bytes per TB fall in inverse proportion to chunk size.
</p>
<p>
	<strong>2. Multi-host benefits.</strong><br />
	Provisioning and migrating a guest between hosts move the manifest plus the uncompacted tail, within 10% of that bound.<br />
	With one copy per chunk, the two-host testbed stores at most 55% of what two per-host deduplication stores hold.<br />
	Two hosts is the floor of this gain: Meyer and Bolosky showed deduplication savings grow with the log of the number of machines in one domain, so a larger fleet gains more, not less.
</p>
<p>
	<strong>3. The cost of a read over the network.</strong><br />
	A chunk served from the owner's memory arrives faster than a local NVMe read on both TCP and RDMA.<br />
	From the owner's NVMe it costs at most 40% over local on TCP and 15% on RDMA.<br />
	With enough reads in flight, remote sequential throughput matches local.
</p>
<p>
	<strong>4. The cost of durability before acknowledgment.</strong><br />
	Fleet class costs one round trip and one peer fdatasync per FLUSH.<br />
	Its write p99 at QD1 is within 3x of local class on TCP, and within 2x on RDMA if the ibverbs arm lands.<br />
	In local class, a lost host loses exactly the acknowledged bytes not yet compacted to an owner, and that window is reported in seconds.
</p>
<p class="note">
	Thresholds come from the transport literature on page 04 and the census prediction on page 02.<br />
	They are frozen at the end of week 2 and do not move.
</p>

<h2>Outputs</h2>
<p>
	<strong>The system.</strong><br />
	A content-addressed block backend for VMs under unmodified QEMU on a stock Linux kernel, over kernel TCP, with source, configuration, and the scripts that produce every table.
</p>
<p>
	<strong>The single-host table.</strong><br />
	Our CAS system against ZFS fast dedup and a raw file on XFS: bytes stored, guest p99, write amplification, and index memory, at three chunk sizes.<br />
	This is where hypothesis 1 is decided and where the chunk-size trade-off is measured.
</p>
<p>
	<strong>The multi-host table.</strong><br />
	Bytes moved to provision and to migrate a guest, bytes sent to synchronize two drifted guests, and fleet bytes stored with one copy per chunk, each against what <code>zfs send</code> or rsync would move and what two per-host ZFS pools would hold.<br />
	No local-disk backend can produce these numbers.
</p>
<p>
	<strong>The remote-read measurement.</strong><br />
	A content-addressed chunk fetched from a peer under a VM block device, at microsecond resolution, over the daemon on kernel TCP and over NVMe-oF on TCP and RDMA, from the peer's memory and from its NVMe, with and without prefetch.<br />
	No published system has this measurement.
</p>
<p>
	<strong>The durability trade.</strong><br />
	Local class against fleet class on the same hardware: the write latency fleet class costs per transport, and the seconds of acknowledged data local class puts at risk.
</p>

<h2>Scope</h2>
<p>
	The study covers hosts that serve guests from local flash, from a homelab up to rack scale.<br />
	Storage arrays and hyperscale economics are out of scope.
</p>
<p>
	Chunks are placed over N hosts by rendezvous hashing: every host scores each (chunk, host) pair with one hash function, and the k highest-scoring hosts own the chunk.<br />
	Every host computes the same answer with no shared state, no ring, and no lookup, which is what makes it simpler than a consistent-hashing ring at small N.<br />
	The testbed is two hosts with static membership, so failure detection, rebalancing, and authentication are out of scope.<br />
	One copy per chunk (k = 1) on two hosts is a measurement configuration that maximizes remote reads so their cost can be seen; a deployment runs k ≥ 2 on N ≥ 3 hosts.
</p>
<p>
	Each image has one writer.<br />
	The study migrates disks only; memory migration is QEMU's.
</p>
<p>
	The guest contract is virtio-blk with a volatile write cache: an acknowledged FLUSH is durable and nothing else is.<br />
	Every configuration runs with QEMU <code>cache=none</code>, so the host page cache is bypassed everywhere.
</p>
<p>
	Equal BLAKE3 hashes are taken to mean equal bytes; a sample of matches is verified byte for byte and the sample size is reported.<br />
	The store is trusted infrastructure, so deduplication side channels are documented and excluded.
</p>
<p>
	Experiments run at single-digit TB, and larger figures are formulas with measured constants, labeled as such.<br />
	RDMA is a measurement arm on page 04; nothing in the architecture requires it.
</p>

<PageNav num="00" />
