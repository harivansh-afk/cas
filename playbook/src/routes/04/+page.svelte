<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
	import { Diagram, Node, Note, Bracket } from '$lib/components/diagram';
</script>

<PageHead num="04" />
<p class="lede">
	Page 04 measures the one place the network enters guest latency in local class, a cold read whose chunk lives on another host, then how much of it prefetch removes, then the round trip fleet class adds to FLUSH.
</p>

<h2>Where the time goes in a remote read</h2>
<p>
	A 4 KiB random read at QD1 from an enterprise NVMe SSD completes in about 80 µs: 81.6 µs on a PM1725 in <a href="https://www.systor.org/2017/slides/NVMe-over-Fabrics_Performance_Characterization.pdf" target="_blank" rel="noopener">Systor '17</a> and about 80 µs on a PM1735 in <a href="https://www.usenix.org/system/files/osdi21-hwang.pdf" target="_blank" rel="noopener">blk-switch</a>. R0 measures the testbed's drive in week 1.<br />
	On 100 GbE the transport sits on top.<br />
	Against a null device, kernel nvme-rdma added 12.1 µs and kernel nvme-tcp 21.4 µs for a 4 KiB read on ConnectX-5 (<a href="https://review.spdk.io/download/performance-reports/SPDK_rdma_mlx_perf_report_2405.pdf" target="_blank" rel="noopener">SPDK 24.05</a>). A raw RDMA round trip is 2 to 5 µs, from 32 bytes to 4 KiB (<a href="https://arxiv.org/pdf/1806.00680" target="_blank" rel="noopener">eRPC</a>; SPDK). On two CloudLab c6525-100g nodes, the testbed's node type, <a href="https://arxiv.org/pdf/2312.06808" target="_blank" rel="noopener">BPF-oF</a> measured average round trips of 18 µs over nvme-rdma and 30 µs over nvme-tcp on kernel 5.12.<br />
	A userspace daemon over kernel TCP has no published measurement as a remote read target. From the kernel TCP round-trip floor of 13 to 23 µs (<a href="https://www.usenix.org/system/files/atc21-ousterhout.pdf" target="_blank" rel="noopener">Homa</a>; <a href="https://www.cs.cornell.edu/~ragarwal/pubs/understanding-latency.pdf" target="_blank" rel="noopener">Zuo et al.</a>) plus a file read, we estimate 20 to 30 µs when it polls and more when it sleeps.<br />
	The testbed replaces every one of these figures.
</p>
<p>
	On these figures the difference between RDMA and TCP is about 9 µs on a read of about 100 µs.<br />
	The larger factor, about 4x, is whether the chunk is in the owner's memory or on its disk.<br />
	<mark>If the figures hold, a chunk from a peer's memory over TCP arrives before one from local NVMe</mark>, and hypothesis 3 tests this.<br />
	Every host sends its reads of a chunk to the same k owners, so we predict a chunk read by many guests is hot at its owner, and a remote read in that case is the memory row. The owner's hit rate is reported under the boot storm below.
</p>
<p class="note">
	A caution from a prior implementation by the author: a peer round trip over QUIC with TLS on a bonded 25 GbE link measured 108 µs at p50 and 257 µs at p99 (unpublished). The daemon here uses kernel TCP with <code>TCP_NODELAY</code> on 100 GbE.
</p>

<Diagram
	w={960}
	h={260}
	label="Horizontal bars, one per case, length proportional to latency from the literature. Local NVMe about 80 microseconds. A null target over kernel nvme-rdma about 12 and over nvme-tcp about 21, the stack alone. Peer memory over the daemon on TCP 20 to 30, estimated. Peer NVMe over nvme-rdma about 92, over nvme-tcp about 101, over the daemon about 110. The stack-only and peer-memory bars are all shorter than the local NVMe bar."
	caption="Literature values for one 4 KiB read at QD1, in microseconds, before the testbed measures them. The daemon rows are estimates."
>
	<Note x={20} y={36} tone="muted" size={10} text="local NVMe read" />
	<Node x={215} y={22} w={320} h={20} title="≈ 80 µs" tone="muted" />

	<Note x={20} y={80} tone="accent" size={10} text="null target over nvme-rdma, stack alone" />
	<Node x={215} y={66} w={64} h={20} title="≈ 12 µs" tone="accent" />
	<Note x={20} y={110} tone="accent" size={10} text="null target over nvme-tcp, stack alone" />
	<Node x={215} y={96} w={84} h={20} title="≈ 21 µs" tone="accent" />
	<Note x={20} y={140} tone="accent" size={10} text="peer memory, daemon on TCP, estimate" />
	<Node x={215} y={126} w={100} h={20} title="20 to 30 µs" tone="accent" />
	<Bracket x={335} y1={66} y2={146} label={['shorter than the local NVMe bar', 'the case hash placement makes common']} tone="accent" />

	<Note x={20} y={184} size={10} text="peer NVMe over RDMA" />
	<Node x={215} y={170} w={368} h={20} title="≈ 92 µs" tone="outline" />
	<Note x={20} y={214} size={10} text="peer NVMe over nvme-tcp" />
	<Node x={215} y={200} w={404} h={20} title="≈ 101 µs" tone="outline" />
	<Note x={20} y={244} size={10} text="peer NVMe, daemon on TCP" />
	<Node x={215} y={230} w={440} h={20} title="≈ 110 µs" tone="outline" />
	<Bracket x={695} y1={170} y2={250} label={['15 to 40% over local', 'the cold case; prefetch is measured against it']} />
</Diagram>

<h2>Transport probes</h2>
<p>
	The architecture's transport is the daemon over kernel TCP.<br />
	The other rows exist to show what the kernel stack and the userspace hop each cost.
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
	The nvmet export is a probe and not the architecture. It exposes the raw store, needs the reader to know offsets, and has no place for authentication.<br />
	It is in the table because the difference between it and the daemon over the same TCP is the cost of the userspace hop, with the delta between SPDK's userspace target and the kernel's, 1 µs on TCP and 2.7 µs on RDMA in the 24.05 reports, as the reference point.
</p>

<h2>Method</h2>
<ul class="plain">
	<li>Same two hosts, NIC, drive, and kernel for every row. Kernel, firmware, MTU, IRQ affinity, interrupt moderation, C-states, busy-poll, and PFC state recorded.</li>
	<li>The link is measured before any remote number: <code>ib_read_lat</code> for the RDMA floor and a TCP ping-pong for the kernel floor, both recorded beside the rows.</li>
	<li>Two targets per row: a null device for fabric plus stack alone, and the real file for end to end. Each from the owner's memory and from its NVMe.</li>
	<li>Two load states for the file rows: quiet, and with <code>PUT</code> traffic running on its own connection at the ship rate from page 03, because a cold read in deployment competes with compaction. The difference is what the read-priority rule on page 01 buys.</li>
	<li>4 KiB, 16 KiB, 64 KiB. p50, p99, p99.9. Five runs of 30 s, caches dropped between, medians with spread.</li>
	<li>QD sweep 1, 4, 16, 64 for throughput and CPU per IOPS on both ends. Kernel TCP costs 2.5 to 3x the CPU of RDMA at equal IOPS in the SPDK 24.05 reports and in <a href="https://www.usenix.org/system/files/nsdi20-paper-hwang.pdf" target="_blank" rel="noopener">i10</a>, and the ratio measured here is reported.</li>
	<li>RoCE hardware counters (<code>out_of_sequence</code>, <code>packet_seq_err</code>, <code>local_ack_timeout_err</code>) printed beside every RDMA number, showing zero retransmits on a fabric with no PFC.</li>
</ul>

<h2>Prefetch</h2>
<p>
	The depth sweep reads sequentially through the manifest with P chunks in flight, P doubling from 1 to 512 at 4 KiB and from 1 to 32 at 64 KiB.<br />
	The bandwidth-delay point is about 250 KB for the fabric (100 Gb/s × 20 µs) and about 1.2 MB with media under it (100 Gb/s × 100 µs), so we predict that about 20 chunks of 64 KiB or 300 of 4 KiB in flight hide the remote read.<br />
	Success is remote sequential throughput within 10% of local, the throughput clause of hypothesis 3.
</p>
<p>
	Profile prefetch records the chunk sequence of one boot and replays it on later boots.<br />
	DADI, REAP, FaaSnap, VMTorrent, and Nydus each prefetch a recorded access profile (page 06).<br />
	It is budgeted at one day.
</p>

<h2>Under a guest workload</h2>
<p>
	Partitioned boot storm at n = 16, with and without profile prefetch, against the same storm in replicated mode.<br />
	The run reports guest p99, host device reads per guest byte, the fraction of reads served by the peer, and the fraction of those the peer answered from memory, so the per-read cost and the miss rate can be multiplied and the hotness prediction on page 00 is checked.<br />
	<mark>The gap between partitioned with prefetch and replicated is the residual cost of one copy per chunk.</mark>
</p>

<h2>The FLUSH round trip</h2>
<p>
	Fleet class puts one round trip and one remote fdatasync in front of every FLUSH acknowledgment. On the figures above the transport is about a tenth of a cold read over RDMA and a fifth over TCP, because 80 µs of media sits beneath the read. A FLUSH has no media to hide behind, so we predict the round trip and the peer's fdatasync are most of its cost, and hypothesis 4 bounds it.<br />
	It is measured here with the same discipline as the read rows: write p99 at QD1 for local class, for fleet class over the daemon on TCP, and for fleet class over ibverbs if that arm lands, with the peer's fdatasync time reported separately so the transport's share is visible.
</p>

<h2>RDMA on this testbed</h2>
<p>
	The CloudLab fabric is lossy. No PFC or ECN is documented on the shared switches, and the one published RoCE measurement on this node type does not say whether either was on.<br />
	Adaptive retransmission is enabled on the NIC and the counters above show whether the runs were clean.<br />
	ConnectX-5 cannot do io_uring zero-copy receive, so that option is unavailable.
</p>

<PageNav num="04" />
