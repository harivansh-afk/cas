<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
	import { Diagram, Node, Edge, Group, Note } from '$lib/components/diagram';
</script>

<PageHead num="03" />
<p class="lede">
	Page 03 runs the backend on two hosts with one parameter, k, and measures bytes moved and bytes stored against what <code>zfs send</code>, rsync, and two per-host ZFS pools would move and hold.
</p>

<h2>Two placement modes</h2>
<p>
	k, the number of owners per chunk (page 01), takes two values on two hosts, and they are two different experiments.<br />
	Surviving a dark host at two hosts costs a full mirror of chunks (k = 2) plus fleet class for the staging tail.
</p>

<Diagram
	w={960}
	h={250}
	label="Left, replicated: k equals 2, every chunk is on both hosts, compaction sends each new unique chunk once, and no read is ever remote. Right, partitioned: k equals 1, each chunk lives on the host its hash selects, fleet capacity is one copy per chunk, and one half of a guest's cold reads go to the other host in expectation."
	caption="k = 2 provides transfer savings and keeps every read local. k = 1 provides capacity savings at the cost of remote reads. Two hosts with k = 1 send one half of cold reads to the peer in expectation, the largest share this testbed can produce."
>
	<Group x={20} y={20} w={440} h={210} label="replicated · k = 2" />
	<Node x={40} y={76} w={185} h={60} title="host A" sub={['staging log and manifests', 'every chunk']} />
	<Node x={255} y={76} w={185} h={60} title="host B" sub={['staging log and manifests', 'every chunk']} />
	<Edge points={[[225, 98], [255, 98]]} />
	<Edge points={[[255, 114], [225, 114]]} />
	<Note x={240} y={62} anchor="middle" size={10} text="PUT each new chunk" />
	<Note x={240} y={170} anchor="middle" tone="muted" text={['each unique chunk is transferred once', 'every read is local', 'capacity: the full store on each host']} />

	<Group x={500} y={20} w={440} h={210} label="partitioned · k = 1" tone="accent" />
	<Node x={520} y={76} w={185} h={60} title="host A" sub={['staging log and manifests', 'chunks whose hash selects A']} tone="accent" />
	<Node x={735} y={76} w={185} h={60} title="host B" sub={['staging log and manifests', 'chunks whose hash selects B']} tone="accent" />
	<Edge points={[[705, 98], [735, 98]]} tone="accent" />
	<Edge points={[[735, 114], [705, 114]]} tone="accent" />
	<Note x={720} y={62} anchor="middle" size={10} tone="accent" text="PUT and GET by hash" />
	<Note x={720} y={170} anchor="middle" tone="accent" text={['each chunk is stored on one host', 'one half of cold reads are remote', 'capacity: the store split across hosts']} />
</Diagram>

<h2>Provisioning</h2>
<p>
	A new guest on host B from an image whose chunks exist anywhere costs a copy of the manifest: at least 32 bytes per chunk, about 80 MB for a 40 GB image at 16 KiB chunks. Every chunk it names already exists at its owner.<br />
	In replicated mode no other data is transferred.<br />
	In partitioned mode no other data is transferred either, because chunks are fetched on first read.
</p>
<p>
	The baseline is <code>qemu-img convert</code> or <code>scp</code> of the raw file and <code>zfs send | zfs recv</code> of the zvol, each moving the allocated size of the image.<br />
	Liquid cloned an image by copying its metadata file, in milliseconds. Its distribution benchmark moved an 8 GB image to seven nodes on 1 GbE in 35 s against 730 s by scp, and still moved every unique block to every node. Here provisioning is bytes on the wire at 100 GbE.
</p>

<h2>Migration</h2>
<p>
	To move a guest from A to B, the daemon freezes the device on A and takes E, hands the image to B by one fenced swap of its root record, ships the manifest and the staging extents in (D, E], and resumes on B.<br />
	The root record names the writer and carries a generation number, and the swap is written durably on both hosts before B resumes. A accepts no write after the swap, and B resumes only after the swap names it.<br />
	On resume the log is reconciled by evidence, the replayed E against what is durable on disk, never by who claims to own it. In a prior implementation by the author, a refusal keyed on writer identity kept healthy guests from restarting.<br />
	A 40 GB image that compacted recently moves its manifest, about 80 MB at 16 KiB chunks, plus the staging tail, which was under 9 MB for an idle guest in that implementation and is workload-bound for a busy one.
</p>
<p>
	We predict that bytes are the small part of a migration.<br />
	The disk cut measured 3 to 6 ms in that implementation and the rest of the blackout was orchestration, so the blackout is reported decomposed into freeze, swap, transfer, and resume, beside the bytes. Governor pacing is disabled while the guest is paused.
</p>
<p>
	The baseline is rsync of the raw file and <code>zfs send</code> of the zvol, which since 2.0 emits no deduplicated stream (page 00), so both move the allocated size.
</p>

<h2>Synchronization after drift</h2>
<p>
	Two guests, one on each host, are cloned from the same image and each updated independently to the same package set.<br />
	Compaction on each host sends only the chunks the owner lacks, packed in sealed segments.<br />
	Bytes on the wire are read against the census's unique-byte count for the pair.<br />
	Chunks per second is reported beside bytes per second, the compactor's ship rate, because we predict per-chunk cost caps the path before the link does.<br />
	This is the <code>apt upgrade</code> case from page 00, measured.
</p>

<h2>Capacity</h2>
<p>
	Partitioned mode stores each chunk once across the fleet.<br />
	Bytes on both stores after the fleet replay and the sweep are measured against two per-host ZFS pools holding the same guests, with index bytes on each host. Hypothesis 2 predicts at most 55% of the pools' bytes and about half the index per host.<br />
	The fraction of a guest's cold reads served by the other host is measured too.<br />
	On two hosts with k = 1 that fraction is one half in expectation. In general it is 1 − k/N, so a larger fleet at fixed k sends a larger share of its cold reads over the network, and the two-host number is a lower bound on that share.
</p>

<h2>The local-class window</h2>
<p>
	In local class, between a FLUSH acknowledgment and the chunk being durable at its owner sits the compaction lag, (O, E] in the watermark's terms.<br />
	It is reported in seconds under the fleet replay, as a distribution, with the segment size as the parameter.<br />
	That window is what a lost host loses, and it is the RPO (recovery point objective) of local class.
</p>
<p>
	Fleet class closes the window and pays one round trip and one remote fdatasync per FLUSH. Page 04 measures that cost.
</p>

<h2>Measurements</h2>
<div class="table-scroll">
	<table class="spec prose">
		<thead>
			<tr><th>Flow</th><th>Daemon</th><th>Baseline</th><th>Read against</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">provision</td><td>bytes transferred, both modes</td><td>scp of raw file; zfs send</td><td>manifest size</td></tr>
			<tr><td class="k">migrate</td><td>bytes transferred, both modes; blackout decomposed</td><td>rsync; zfs send</td><td>manifest size + staging tail; milliseconds for the cut</td></tr>
			<tr><td class="k">sync after drift</td><td>bytes and chunks per second sent by compaction</td><td>rsync; zfs send</td><td>census unique bytes</td></tr>
			<tr><td class="k">capacity</td><td>bytes stored, partitioned, after the sweep</td><td>two per-host ZFS pools</td><td>at most 55% of the pools' bytes (hypothesis 2); census prediction</td></tr>
			<tr><td class="k">index per host</td><td>index bytes on each host, both modes</td><td>DDT bytes per pool</td><td>k/N of the fleet index</td></tr>
			<tr><td class="k">remote fraction</td><td>cold reads served by the peer</td><td></td><td>one half in expectation</td></tr>
			<tr><td class="k">local-class window</td><td>seconds from ack to owner-durable</td><td></td><td>the RPO of local class</td></tr>
		</tbody>
	</table>
</div>

<h2>The locality objection</h2>
<p>
	Dong et al. (page 06) rejected per-chunk hash placement for backup streams on locality grounds. This is primary storage with a local cache, so page 04 measures the fragmentation cost directly, and super-chunk placement is the knob if it is large.
</p>

<PageNav num="03" />
