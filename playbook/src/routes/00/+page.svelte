<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
	import { Diagram, Node, Edge, Group, Note } from '$lib/components/diagram';
</script>

<PageHead num="00" />
<p class="lede">
	<strong>Thesis.</strong><br />
	A deduplication table shares duplicate data within one host.<br />
	Content addressing shares it across hosts.<br />
	On Linux VM fleets the first is already solved by ZFS.<br />
	So the case for content addressing rests on what a name does that an address cannot: move only unique bytes between hosts, store each chunk k times across a fleet instead of once per host, and serve a chunk from whichever host holds it in memory.
</p>
<p>
	This study builds that backend on a stock hypervisor and measures what each provides and what each costs.
</p>

<h2>Where deduplication stops</h2>
<p>
	Two VMs each run <code>apt upgrade</code> and download the same packages.<br />
	Their disks now hold the same bytes.<br />
	No clone can share them, because neither copy descends from the other.
</p>
<p>
	A deduplication table can.<br />
	ZFS deduplication and dm-vdo hash every block and share equal ones, at a fixed aligned block size.<br />
	On a Linux guest everything is 4K aligned: ext4 uses 4K blocks, partitions start at 1 MiB, and package managers write whole files.<br />
	So at 4K a deduplication table reaches nearly all of it, which is why Jin and Miller found fixed blocks match content-defined chunking on VM images in 2009.
</p>
<p>
	<mark>On one host, content addressing has no capacity win over ZFS at 4K.</mark><br />
	Part 1 measures this instead of assuming it.
</p>
<p>
	Every local-disk mechanism stops at the host boundary.<br />
	The DDT is per pool.<br />
	<code>zfs send</code> dropped deduplicated streams in 2.0.<br />
	dm-vdo has no replication.<br />
	Reflinks do not survive rsync.<br />
	A clone on host B shares nothing with host A.<br />
	A fleet of N hosts each running deduplication stores every shared chunk N times and moves it whole every time a guest moves.
</p>

<Diagram
	w={960}
	h={284}
	label="Left: two hosts each with their own deduplication table and no link between them; the same chunk is stored on both and moves whole when a guest migrates. Right: two hosts whose chunks are named by hash and owned by hash across both; a chunk is stored k times fleet-wide, a guest's map moves while its chunks stay, and a cold read fetches by name from the owner."
	caption="Left: a deduplication table shares within a host. Right: a name is the same on every host, so placement, transfer, and cache follow the name."
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
	<Note x={720} y={190} anchor="middle" tone="accent" text={['one chunk namespace across hosts', 'a shared chunk is stored k times fleet-wide', 'migration copies the map; the chunks stay', 'a cold read fetches the chunk from its owner']} />
</Diagram>

<h2>What content addressing provides</h2>
<p>
	A chunk named by its hash has the same name on every host, so placement is a function of the name.
</p>
<p>
	Provisioning a guest moves its map, 32 bytes per chunk, and no chunks.<br />
	Migrating a guest moves the map plus whatever it wrote since the last compaction.<br />
	A fleet stores each chunk k times, not once per host.<br />
	A chunk that is hot anywhere is in some host's memory, and <mark>a peer's memory over 100 GbE is closer than local NVMe</mark>: about 20 µs against about 80.
</p>

<h2>The cost</h2>
<p>
	The network sits on the read path for cold chunks and nowhere else.<br />
	Never on the write path, never on FLUSH.<br />
	Part 3 measures the cost of a cold read on TCP and on RDMA, from a peer's memory and from its NVMe, and shows how much of it prefetch hides.
</p>
<p>
	The remaining costs are the ones every deduplication design incurs, and this one measures them: write amplification, compactor interference with the guest, and index memory.
</p>
<p>
	Durability is a class, not a design choice.<br />
	Local class acknowledges after fdatasync on this host and is the default, because that is the contract a local disk gives and the baselines are local disks.<br />
	Fleet class ships the staging tail to a fixed peer and acknowledges after the peer's fdatasync, which is what every hyperconverged product does.<br />
	Both run the same pipeline after the ack; the difference is one round trip per FLUSH, and part 2 measures it on TCP and on RDMA.
</p>

<h2>Hypotheses</h2>
<ul class="reqs">
	<li>
		<span class="rid">H1</span><strong>Single-host parity.</strong><br />
		The daemon stores within 10% of the bytes ZFS fast dedup stores at the same block size, with guest p99 within 20% of a raw file on XFS.<br />
		Index bytes per TB fall in inverse proportion to chunk size.
	</li>
	<li>
		<span class="rid">H2</span><strong>Cross-host benefit.</strong><br />
		Provisioning and migrating a guest between hosts move the map plus the uncompacted tail, within 10% of that bound.<br />
		With one copy per chunk, the two-host testbed stores at most 55% of what two per-host deduplication stores hold.
	</li>
	<li>
		<span class="rid">H3</span><strong>The cost is the remote cold read.</strong><br />
		A chunk served from the owner's memory arrives faster than a local NVMe read on both TCP and RDMA.<br />
		From the owner's NVMe it costs at most 30% over local on TCP and 15% on RDMA.<br />
		With enough reads in flight, remote sequential throughput matches local.
	</li>
	<li>
		<span class="rid">H4</span><strong>The price of durability before ack.</strong><br />
		Fleet class costs one round trip and one peer fdatasync per FLUSH.<br />
		Its write p99 at QD1 is within 3x of local class on TCP and within 2x on RDMA.<br />
		In local class, a lost host loses exactly the acknowledged bytes not yet compacted to an owner, and that window is reported in seconds.
	</li>
</ul>
<p class="note">
	Thresholds come from the transport literature on page 04 and the census prediction on page 02.<br />
	They are frozen at the end of week 2 and do not move.
</p>

<h2>What comes out</h2>
<ul class="plain">
	<li>A working content-addressed block backend under unmodified QEMU, on a stock kernel, over kernel TCP.</li>
	<li>A single-host table against ZFS fast dedup: capture, p99, write amplification, index memory, as a function of chunk size.</li>
	<li>Two numbers no existing backend can match: bytes moved to provision and migrate a guest, and fleet bytes stored with one copy per chunk.</li>
	<li>The first microsecond-scale measurement of a content-addressed chunk fetched from a peer under a VM block device, over kernel TCP and over NVMe-oF on TCP and RDMA.</li>
	<li>The cost of durability before acknowledgment on the same hardware: local class against fleet class, per transport.</li>
</ul>

<h2>Scope</h2>
<ul class="reqs">
	<li><span class="rid">A1</span>Hosts serving guests from local flash, homelab to rack scale. Array economics are out of scope.</li>
	<li><span class="rid">A2</span>The design places chunks over N hosts by rendezvous hashing; the testbed is two hosts with static membership. No failure detection, rebalancing, or authentication. One copy per chunk on two hosts is a measurement configuration; a deployment runs k ≥ 2 on N ≥ 3.</li>
	<li><span class="rid">A3</span>One image, one writer. Disk migration only; memory migration is QEMU's.</li>
	<li><span class="rid">A4</span>The guest contract is virtio-blk with a volatile write cache: an acknowledged FLUSH is durable and nothing else is. Every configuration runs with QEMU <code>cache=none</code>, so the host page cache is bypassed everywhere.</li>
	<li><span class="rid">A5</span>Equal BLAKE3 implies equal bytes. A sample of matches is verified byte for byte and the sample size reported.</li>
	<li><span class="rid">A6</span>The store is trusted infrastructure. Deduplication side channels are documented and excluded.</li>
	<li><span class="rid">A7</span>Experiments run at single-digit TB. Larger figures are formulas with measured constants and are labeled as such.</li>
	<li><span class="rid">A8</span>RDMA is a measurement arm on page 04. Nothing in the architecture requires it.</li>
</ul>

<PageNav num="00" />
