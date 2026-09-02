<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
	import { Diagram, Node, Note, Bracket } from '$lib/components/diagram';
</script>

<PageHead num="04" />
<p class="lede">
	Part 3 measures the one place the network enters guest latency: a cold read whose chunk lives on another host.<br />
	This page measures that read and then reduces it.
</p>

<h2>Where the time goes in a remote read</h2>
<p>
	Every read has about 80 µs of NVMe media time under it.<br />
	On 100 GbE the transport sits on top: raw RDMA adds 3 to 5 µs, kernel nvme-rdma about 12, kernel nvme-tcp about 21, and a userspace daemon over kernel TCP 20 to 30.<br />
	Those are the <a href="https://review.spdk.io/download/performance-reports/SPDK_rdma_mlx_perf_report_2405.pdf" target="_blank" rel="noopener">SPDK 24.05</a> and <a href="https://www.systor.org/2017/slides/NVMe-over-Fabrics_Performance_Characterization.pdf" target="_blank" rel="noopener">Systor '17</a> numbers on ConnectX-5, and the testbed replaces them.
</p>
<p>
	RDMA against TCP is therefore a 10 µs difference on an 80 µs read.<br />
	The larger factor, about 4x, is whether the chunk is in the owner's memory or on its disk.<br />
	If those numbers hold, <mark>a chunk from a peer's memory over TCP arrives faster than one from local NVMe</mark>, and hypothesis 3 tests this.<br />
	With hash placement, a chunk shared across the fleet is hot at exactly one owner, so every host's read of it hits that owner's cache.
</p>

<Diagram
	w={960}
	h={260}
	label="Horizontal bars, one per case, length proportional to latency from the literature. Local NVMe about 80 microseconds. Peer RAM over RDMA about 12, over TCP about 21, over the daemon on TCP 20 to 30. Peer NVMe over RDMA about 92, over TCP about 101, over the daemon about 110. The peer memory bars are all shorter than the local NVMe bar."
	caption="Latency stack for one 4K read, literature values in microseconds, before the testbed measures them. A peer's memory is closer than the local disk on every transport; the transport tier moves the bar by 10 to 30%."
>
	<Note x={20} y={36} tone="muted" size={10} text="local NVMe read" />
	<Node x={215} y={22} w={320} h={20} title="≈ 80 µs" tone="muted" />

	<Note x={20} y={80} tone="accent" size={10} text="peer memory over RDMA" />
	<Node x={215} y={66} w={64} h={20} title="≈ 12 µs" tone="accent" />
	<Note x={20} y={110} tone="accent" size={10} text="peer memory over nvme-tcp" />
	<Node x={215} y={96} w={84} h={20} title="≈ 21 µs" tone="accent" />
	<Note x={20} y={140} tone="accent" size={10} text="peer memory, daemon on TCP" />
	<Node x={215} y={126} w={100} h={20} title="20 to 30 µs" tone="accent" />
	<Bracket x={335} y1={66} y2={146} label={['faster than a local NVMe read', 'the common case under hash placement']} tone="accent" />

	<Note x={20} y={184} size={10} text="peer NVMe over RDMA" />
	<Node x={215} y={170} w={368} h={20} title="≈ 92 µs" tone="outline" />
	<Note x={20} y={214} size={10} text="peer NVMe over nvme-tcp" />
	<Node x={215} y={200} w={404} h={20} title="≈ 101 µs" tone="outline" />
	<Note x={20} y={244} size={10} text="peer NVMe, daemon on TCP" />
	<Node x={215} y={230} w={440} h={20} title="≈ 110 µs" tone="outline" />
	<Bracket x={695} y1={170} y2={250} label={['10 to 30% slower than local', 'the cold case; prefetch hides it']} />
</Diagram>

<h2>Transport probes</h2>
<p>
	The architecture's transport is the daemon over kernel TCP.<br />
	The other rows exist to show what the kernel stack and the userspace hop each cost, and nothing depends on them.
</p>
<div class="table-scroll">
	<table class="spec prose">
		<thead>
			<tr><th>Probe</th><th>What it isolates</th><th>Code</th></tr>
		</thead>
		<tbody>
			<tr><td class="k"><code>ib_read_lat -s 4096</code></td><td>the hardware floor</td><td>none</td></tr>
			<tr><td class="k">nvme-rdma export</td><td>kernel block path over RDMA; owner's store exported by <code>nvmet</code> as a file-backed namespace, <code>buffered_io</code> on for memory, off for media</td><td>configuration</td></tr>
			<tr><td class="k">nvme-tcp export</td><td>same over kernel TCP</td><td>configuration</td></tr>
			<tr><td class="k">daemon, TCP, busy-polling</td><td>the architecture, without the wakeup</td><td>the daemon</td></tr>
			<tr><td class="k">daemon, TCP, blocking</td><td>the architecture as deployed; the scheduler wakeup is the cost</td><td>the daemon</td></tr>
			<tr><td class="k">daemon, ibverbs two-sided<span class="tag-stretch">stretch</span></td><td>the userspace hop without the kernel stack</td><td>~40 h</td></tr>
		</tbody>
	</table>
</div>
<p>
	The nvmet export is a probe and not the architecture: it exposes the raw store, needs the reader to know offsets, and has no place for authentication.<br />
	It is in the table because the difference between it and the daemon over the same TCP is the cost of the userspace hop, with SPDK's 1 µs kernel-versus-userspace target delta as the reference point.
</p>

<h2>Method</h2>
<ul class="plain">
	<li>Same two hosts, NIC, drive, and kernel for every row. Kernel, firmware, MTU, IRQ affinity, interrupt moderation, C-states, busy-poll, and PFC state recorded.</li>
	<li>Two targets per row: a null device for fabric plus stack alone, and the real file for end to end. Each from the owner's memory and from its NVMe.</li>
	<li>Two load states for the file rows: quiet, and with <code>PUT</code> traffic running on its own connection at the ship rate from page 03, because a cold read in deployment competes with compaction. The difference is what the read-priority rule on page 01 buys.</li>
	<li>4K, 16K, 64K. p50, p99, p99.9. Five runs of 30 s, caches dropped between, medians with spread.</li>
	<li>QD sweep 1, 4, 16, 64 for throughput and CPU per IOPS on both ends; TCP costs about 2.5x the CPU of RDMA at equal IOPS and the paper shows the ratio it measures.</li>
	<li>RoCE hardware counters (<code>out_of_sequence</code>, <code>packet_seq_err</code>, <code>local_ack_timeout_err</code>) printed beside every RDMA number, proving zero retransmits on a fabric with no PFC.</li>
</ul>

<h2>Prefetch</h2>
<p>
	The manifest tells the daemon what comes next.<br />
	Depth sweep: sequential reads through the manifest with 1, 2, 4, 8, 16, 32 chunks in flight, at 4K and 64K.<br />
	The bandwidth-delay point is about 250 KB for the fabric and about 1 MB with media under it, so roughly 20 chunks of 64K or 300 of 4K outstanding should hide the remote entirely.<br />
	Success is remote sequential throughput within the error bars of local.
</p>
<p>
	Profile prefetch: record the chunk sequence of one boot, replay it on later boots.<br />
	Every lazy-loading system that has published numbers does this and reports it removing most of the miss cost; <a href="https://www.usenix.org/system/files/atc20-li-huiba.pdf" target="_blank" rel="noopener">DADI</a> says 95%.<br />
	It is the consensus mitigation and a one-day implementation.
</p>

<h2>Under a guest workload</h2>
<p>
	Partitioned boot storm at N = 16, with and without profile prefetch, against the same storm in replicated mode.<br />
	Reported: guest p99 and host device reads per guest byte.<br />
	<mark>The gap between partitioned with prefetch and replicated is the residual cost of one copy per chunk.</mark>
</p>

<h2>The FLUSH round trip</h2>
<p>
	Fleet class on page 03 puts one round trip and one remote fdatasync in front of every FLUSH acknowledgment, and there is no 80 µs of media to hide behind, so the round trip and the peer's fdatasync are the whole cost.<br />
	It is measured here with the same discipline as the read rows: write p99 at QD1 for local class, for fleet class over the daemon on TCP, and for fleet class over ibverbs if that arm lands, with the peer's fdatasync time reported separately so the transport's share is visible.
</p>

<h2>RDMA on this testbed</h2>
<p>
	The CloudLab fabric is lossy: no PFC or ECN is documented on the shared switches, and published work on this node type ran RoCE that way.<br />
	Adaptive retransmission is enabled on the NIC and the counters above prove the runs were clean.<br />
	ConnectX-5 cannot do io_uring zero-copy receive, so that option is unavailable.<br />
	None of this touches the architecture, which runs on kernel TCP and would run on any Ethernet.
</p>

<h2>Hypothesis 3, restated</h2>
<ul class="plain">
	<li>A chunk from the owner's memory arrives faster than a local NVMe read, on TCP and on RDMA.</li>
	<li>From the owner's NVMe it costs at most 40% over local on TCP and 15% on RDMA, at QD1, 4K.</li>
	<li>At depth at or above the bandwidth-delay point, remote sequential throughput is within 10% of local.</li>
	<li>Partitioned boot storm p99 with profile prefetch is within 25% of replicated.</li>
</ul>

<PageNav num="04" />
