<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
	import { Diagram, Node, Edge, Group, Note } from '$lib/components/diagram';
</script>

<PageHead num="00" />
<p class="lede">
	<strong>Thesis.</strong>
	A dedup table shares duplicate data within one host.
	Content addressing shares it across hosts.
	On Linux VM fleets the first is already solved by ZFS.
	So the case for content addressing is what a name does that an address cannot: move only unique bytes between hosts, store each chunk k times across a fleet instead of once per host, and serve a chunk from whichever host holds it in memory.
</p>
<p>
	This study builds that backend on a stock hypervisor and measures what those buy and what they cost.
</p>

<h2>Where dedup stops</h2>
<p>
	Two VMs each run <code>apt upgrade</code> and download the same packages.
	Their disks now hold the same bytes.
	No clone can share them, because neither copy descends from the other.
</p>
<p>
	A dedup table can.
	ZFS dedup and dm-vdo hash every block and share equal ones, at a fixed aligned block size.
	On a Linux guest everything is 4K aligned: ext4 uses 4K blocks, partitions start at 1 MiB, and package managers write whole files.
	So at 4K a dedup table reaches nearly all of it, which is why Jin and Miller found fixed blocks match content-defined chunking on VM images in 2009.
</p>
<p>
	<mark>On one host, content addressing has no capacity win over ZFS at 4K.</mark>
	Part 1 measures this instead of assuming it.
</p>
<p>
	Every stock mechanism stops at the host boundary.
	The DDT is per pool.
	<code>zfs send</code> dropped deduplicated streams in 2.0.
	dm-vdo has no replication.
	Reflinks do not survive rsync.
	A clone on host B shares nothing with host A.
	A fleet of N hosts each running dedup stores every shared chunk N times and moves it whole every time a guest moves.
</p>

<Diagram
	w={960}
	h={300}
	label="Left: two hosts each with their own dedup table and no link between them; the same chunk is stored on both and moves whole when a guest migrates. Right: two hosts whose chunks are named by hash and owned by hash across both; a chunk is stored k times fleet-wide, a guest's map moves while its chunks stay, and a cold read fetches by name from the owner."
	caption="Left: a dedup table shares within a host. Right: a name is the same on every host, so placement, transfer, and cache follow the name."
>
	<Group x={20} y={20} w={440} h={250} label="per-host dedup" />
	<Node x={50} y={60} w={170} h={44} title="host A" sub="guests · ZFS" tone="muted" />
	<Node x={50} y={120} w={170} h={44} title="DDT A" sub="hash → address in pool A" />
	<Node x={260} y={60} w={170} h={44} title="host B" sub="guests · ZFS" tone="muted" />
	<Node x={260} y={120} w={170} h={44} title="DDT B" sub="hash → address in pool B" />
	<Note x={240} y={200} anchor="middle" tone="muted" text={['no link between the tables', 'shared chunk stored twice · moves whole', 'migration = full logical size']} />

	<Group x={500} y={20} w={440} h={250} label="content addressing" tone="accent" />
	<Node x={530} y={60} w={170} h={44} title="host A" sub="guests · daemon" tone="muted" />
	<Node x={530} y={120} w={170} h={44} title="chunks A" sub="owner = f(hash)" tone="accent" />
	<Node x={740} y={60} w={170} h={44} title="host B" sub="guests · daemon" tone="muted" />
	<Node x={740} y={120} w={170} h={44} title="chunks B" sub="owner = f(hash)" tone="accent" />
	<Edge points={[[700, 134], [740, 134]]} tone="accent" label="GET · PUT by name" labelDy={-8} />
	<Edge points={[[740, 150], [700, 150]]} tone="accent" />
	<Note x={720} y={200} anchor="middle" tone="accent" text={['one namespace across hosts', 'shared chunk stored k times · map moves, chunks stay', 'cold read = fetch by name from the owner']} />
</Diagram>

<h2>What a name buys</h2>
<p>
	A chunk named by its hash has the same name on every host, so placement is a function of the name.
</p>
<p>
	Provisioning a guest moves its map, a few MB of offset to hash pairs, and no chunks.
	Migrating a guest moves the map plus whatever it wrote since the last compaction.
	A fleet stores each chunk k times, not once per host.
	A chunk that is hot anywhere is in some host's memory, and <mark>a peer's memory over 100 GbE is closer than local NVMe</mark>: about 20 µs against about 80.
</p>

<h2>The price</h2>
<p>
	The network sits on the read path for cold chunks and nowhere else.
	Never on the write path, never on FLUSH.
	Part 3 prices a cold read on TCP and on RDMA, from a peer's RAM and from its NVMe, and shows how much of it prefetch hides.
</p>
<p>
	The rest is what every dedup design pays and this one measures: write amplification, compactor interference with the guest, index memory, and the window between a local ack and the chunk being durable on its owner.
</p>

<h2>Hypotheses</h2>
<ul class="reqs">
	<li>
		<span class="rid">H1</span><strong>One host is a tie.</strong>
		The daemon stores within 10% of the bytes ZFS fast dedup stores at the same block size, with guest p99 within 20% of a raw file on XFS.
		Index bytes per TB fall in proportion to chunk size.
	</li>
	<li>
		<span class="rid">H2</span><strong>Crossing hosts is where the name pays.</strong>
		Provisioning and migrating a guest between hosts move the map plus the uncompacted tail, within 10% of that bound.
		With one copy per chunk, two hosts store at most 55% of what two per-host dedup stores hold.
	</li>
	<li>
		<span class="rid">H3</span><strong>The price is a cold read, and a peer's RAM beats local disk.</strong>
		A chunk served from the owner's memory arrives faster than a local NVMe read on both TCP and RDMA.
		From the owner's NVMe it costs at most 30% over local on TCP and 15% on RDMA.
		With enough reads in flight, remote sequential throughput matches local.
	</li>
</ul>
<p class="note">
	Thresholds come from the transport literature on page 04 and the census prediction on page 02.
	They are frozen at the end of week 2 and do not move.
</p>

<h2>What comes out</h2>
<ul class="plain">
	<li>A working content-addressed block backend under unmodified QEMU, on a stock kernel, over kernel TCP.</li>
	<li>A single-host table against ZFS fast dedup: capture, p99, write amplification, index memory, as a function of chunk size.</li>
	<li>Two numbers no stock backend can produce: bytes moved to provision and migrate a guest, and fleet bytes stored with one copy per chunk.</li>
	<li>The first microsecond-scale measurement of a content-addressed chunk fetched from a peer under a VM block device, on four transports.</li>
</ul>

<h2>Scope</h2>
<ul class="reqs">
	<li><span class="rid">A1</span>Hosts serving guests from local flash, homelab to rack scale. Array economics are out of scope.</li>
	<li><span class="rid">A2</span>Two hosts with static membership. No failure detection, rebalancing, or authentication. One copy per chunk on two hosts is a measurement configuration; a deployment runs k ≥ 2 on N ≥ 3.</li>
	<li><span class="rid">A3</span>One image, one writer. Disk migration only; memory migration is QEMU's.</li>
	<li><span class="rid">A4</span>The guest contract is virtio-blk with a volatile write cache: an acknowledged FLUSH is durable and nothing else is. Every rung runs under the same QEMU cache mode.</li>
	<li><span class="rid">A5</span>Equal BLAKE3 implies equal bytes. A sample of matches is verified byte for byte and the sample size reported.</li>
	<li><span class="rid">A6</span>The store is trusted infrastructure. Dedup side channels are documented and excluded.</li>
	<li><span class="rid">A7</span>Experiments run at single-digit TB. Larger figures are formulas with measured constants and are labeled as such.</li>
	<li><span class="rid">A8</span>RDMA is a measurement arm on page 04. Nothing in the architecture requires it.</li>
</ul>

<PageNav num="00" />
