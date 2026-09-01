<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
	import { Diagram, Node, Edge, Group, Note } from '$lib/components/diagram';
</script>

<PageHead num="03" />
<p class="lede">
	<strong>Part 2.</strong><br />
	Same daemon, a second host, one parameter.<br />
	Everything a stock backend cannot do is on this page.
</p>

<h2>Two modes</h2>
<p>
	k is the number of owners per chunk.<br />
	On two hosts it takes two values, and they are two different experiments.
</p>

<Diagram
	w={960}
	h={300}
	label="Left, replicated: k equals 2, every chunk is on both hosts, compaction ships each new unique chunk once, and no read is ever remote. Right, partitioned: k equals 1, each chunk lives on the host its hash selects, fleet capacity is one copy per chunk, and about half of a guest's cold reads go to the other host."
	caption="k = 2 buys transfer and keeps every read local. k = 1 buys capacity and pays in remote reads. Two hosts with k = 1 is the worst case for remote reads and is run for exactly that reason."
>
	<Group x={20} y={20} w={440} h={250} label="replicated · k = 2" />
	<Node x={50} y={70} w={170} h={60} title="host A" sub={['staging · maps', 'all chunks']} />
	<Node x={260} y={70} w={170} h={60} title="host B" sub={['staging · maps', 'all chunks']} />
	<Edge points={[[220, 92], [260, 92]]} label="PUT new chunks" labelDy={-8} />
	<Edge points={[[260, 112], [220, 112]]} />
	<Note x={240} y={180} anchor="middle" tone="muted" text={['each unique chunk crosses the wire once', 'every read is local', 'capacity = one store, twice']} />

	<Group x={500} y={20} w={440} h={250} label="partitioned · k = 1" tone="accent" />
	<Node x={530} y={70} w={170} h={60} title="host A" sub={['staging · maps', 'chunks with hash → A']} tone="accent" />
	<Node x={740} y={70} w={170} h={60} title="host B" sub={['staging · maps', 'chunks with hash → B']} tone="accent" />
	<Edge points={[[700, 92], [740, 92]]} tone="accent" label="PUT · GET" labelDy={-8} />
	<Edge points={[[740, 112], [700, 112]]} tone="accent" />
	<Note x={720} y={180} anchor="middle" tone="accent" text={['each chunk stored once fleet-wide', 'about half of cold reads are remote', 'capacity = one store, once']} />
</Diagram>

<h2>Provision</h2>
<p>
	A new guest on host B from an image whose chunks exist anywhere: copy the map, a few MB, and <code>HAS</code> its hashes.<br />
	In replicated mode nothing else moves.<br />
	In partitioned mode nothing else moves either; chunks are fetched on first read.<br />
	<mark>Provisioning cost is the size of the map.</mark>
</p>
<p>
	Baseline: <code>qemu-img convert</code> or <code>scp</code> of the raw file, and <code>zfs send | zfs recv</code> of the zvol, each moving the full logical size.
</p>

<h2>Migrate</h2>
<p>
	Move a guest from A to B: stop, copy the map and the staging extents not yet compacted, start.<br />
	A 40 GB guest that compacted recently moves in MB.<br />
	Memory migration is QEMU's and is out of scope; this is the disk.
</p>
<p>
	Baseline: rsync of the raw file, <code>zfs send</code> of the zvol.<br />
	Since 2.0 <code>zfs send</code> emits no deduplicated stream; the bytes are the logical size regardless of the DDT.
</p>

<h2>Sync after drift</h2>
<p>
	Two guests, one on each host, cloned from the same image, each updated independently to the same package set.<br />
	Compaction on each host ships only chunks the owner lacks.<br />
	Bytes on the wire are read against the census's unique-byte count for the pair.<br />
	This is the <code>apt upgrade</code> case from page 00, measured.
</p>

<h2>Capacity</h2>
<p>
	Partitioned mode stores each chunk once across the fleet.<br />
	Measured: bytes on both stores after the fleet replay settles, against two per-host ZFS pools holding the same guests.<br />
	Predicted: about half.<br />
	Also measured: what fraction of a guest's cold reads went to the other host, which on two hosts with k = 1 should be about half and is the worst case any fleet would see.
</p>

<h2>The window</h2>
<p>
	Between a local FLUSH ack and the chunk being durable on its owner sits the compaction lag.<br />
	It is reported in seconds under the fleet replay, as a distribution, with the compactor's ship batch size as the knob.
</p>
<p>
	Optional arm: mirror the staging tail to the peer on every FLUSH and wait for the peer's fdatasync before acking.<br />
	Every production system in this space does this.<br />
	The arm reports the write p99 it costs on TCP, which is one round trip per FLUSH.
</p>

<h2>Measured</h2>
<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Flow</th><th>Daemon</th><th>Baseline</th><th>Read against</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">provision</td><td>bytes on the wire, both modes</td><td>scp of raw file; zfs send</td><td>map size</td></tr>
			<tr><td class="k">migrate</td><td>bytes on the wire, both modes</td><td>rsync; zfs send</td><td>map size + staging tail</td></tr>
			<tr><td class="k">sync after drift</td><td>bytes shipped by compaction</td><td>rsync; zfs send</td><td>census unique bytes</td></tr>
			<tr><td class="k">capacity</td><td>bytes stored, partitioned</td><td>two per-host ZFS pools</td><td>census prediction</td></tr>
			<tr><td class="k">remote fraction</td><td>cold reads served by the peer</td><td></td><td>about half, worst case</td></tr>
			<tr><td class="k">window</td><td>seconds from ack to owner-durable</td><td>mirror arm: write p99 with mirroring</td><td>one RTT per FLUSH</td></tr>
		</tbody>
	</table>
</div>

<h2>The locality objection</h2>
<p>
	Dong et al. (FAST '11) rejected per-chunk hash placement for backup because it destroys read locality and routed 1 MB super-chunks instead.<br />
	This is primary storage with a local cache, and the fragmentation cost they argued about is measured directly on page 04 instead of argued.<br />
	If it is large, placement by super-chunk is the knob, noted here and measured only if time remains.
</p>

<PageNav num="03" />
