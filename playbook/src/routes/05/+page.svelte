<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="05" />
<p class="lede">
	<strong>Fourteen weeks, about 320 hours.</strong>
	That is 23 a week.
	The credit says 8.
	The plan is sized to the work, and the cut order says what goes when it slips.
</p>

<h2>Hardware</h2>
<p>
	Two CloudLab c6525-100g nodes (Utah), reserved as a pair.
	Per node: AMD EPYC 7402P, 24 cores at 2.80 GHz; 128 GB ECC DDR4-3200; two 1.6 TB PCIe 4.0 NVMe SSDs; ConnectX-5 Ex 100 GbE, one port on the experiment network.
	One NVMe holds the system and results; the other is the device under test.
	The pair is one hop through a single switch.
</p>
<p>
	RoCE between two of these nodes works and has been used in published work on this exact hardware, on a lossy fabric.
	Self-built kernels are routine there; the Ubuntu 24.04 image ships 6.8, dm-vdo needs 6.9, and OpenZFS 2.3 is a source build, so a kernel and ZFS are built once in week 1 and snapshotted as an image.
	Reservations expire at 16 hours by default, so every run is scripted to complete inside one.
</p>
<p>
	CloudLab is free for research.
	A project is opened by a faculty member and reviewed by CloudLab staff; the sponsor opens it before Sep 9.
	Fallback: two OVHcloud Advance-4 2026 servers (EPYC 4585PX, 16 cores, 64 GB DDR5 ECC, 2 × 960 GB NVMe) on a 25 Gbps private link, which loses the RDMA arm and nothing else.
</p>

<h2>Schedule</h2>
<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Weeks</th><th>Build</th><th>Measure</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">1–2</td><td>vhost-user-blk daemon in passthrough: staging log, FLUSH, replay. Kernel and ZFS image.</td><td>R0; passthrough within 10% of R0 p99 (G1). Thresholds frozen. <code>zdb -S</code> phase 0 on the synthetic fleet.</td></tr>
			<tr><td class="k">3–5</td><td>Compactor, store, index, maps, epochs, recovery. Three chunk-size arms.</td><td><code>kill -9</code> recovery passes (G2). First capture numbers.</td></tr>
			<tr><td class="k">6–7</td><td>R1 configured, both volblocksize arms. R2 if hours allow.</td><td>Part 1 table complete (G3).</td></tr>
			<tr><td class="k">8–9</td><td>Protocol, rendezvous placement, k, PUT with durable ack, HAS, one-shot GC. Provision and migrate scripts.</td><td>Replicated mode on two nodes.</td></tr>
			<tr><td class="k">10</td><td>Partitioned mode. Mirror arm if hours allow.</td><td>Part 2 table complete (G4).</td></tr>
			<tr><td class="k">11–12</td><td>nvmet exports, RoCE bring-up, spinning and sleeping daemon, depth prefetch, profile prefetch.</td><td>Transport matrix and prefetch sweeps (G5). Partitioned boot storm.</td></tr>
			<tr><td class="k">13–14</td><td></td><td>Report; reproducibility pack (G6).</td></tr>
		</tbody>
	</table>
</div>

<h2>Gates</h2>
<ul class="reqs">
	<li><span class="rid">G1</span>Passthrough daemon under stock QEMU within 10% of R0 p99 by the end of week 2. If this slips, everything slips, and the sponsor hears it that week.</li>
	<li><span class="rid">G2</span><code>kill -9</code> at arbitrary points, replay, <code>fio --verify</code> passes, before any daemon number is reported.</li>
	<li><span class="rid">G3</span>Part 1 table complete: R0, R1 at two block sizes, R4 at three chunk sizes; latency, capture, index, amplification; variance beside every number.</li>
	<li><span class="rid">G4</span>Part 2 table complete: both modes, every flow, bytes on the wire against the census bound.</li>
	<li><span class="rid">G5</span>Transport matrix complete for the four non-stretch probes, null and file, RAM and NVMe, with RoCE counters at zero.</li>
	<li><span class="rid">G6</span>One command rebuilds the fleet from dated archives; one command reruns every table on a fresh pair.</li>
</ul>

<h2>Cut order</h2>
<p>
	When the schedule slips, items come off from the top.
	Never the item at the bottom.
</p>
<ol class="steps">
	<li>ibverbs daemon arm.</li>
	<li>Super-chunk placement.</li>
	<li>Mirror-on-FLUSH arm.</li>
	<li>R2 dm-vdo.</li>
	<li>Profile prefetch (depth prefetch stays).</li>
	<li>Partitioned mode. Replicated mode alone still gives H2's transfer result.</li>
	<li>Never: part 1, the nvmet TCP and RDMA probes, the daemon over TCP.</li>
</ol>

<h2>Risks</h2>
<ul class="plain">
	<li><strong>Daemon overrun.</strong> The largest risk and the reason G1 is at week 2. Protocol plumbing comes from maintained crates so the hours go to the five components the study is about.</li>
	<li><strong>RoCE bring-up.</strong> GID selection, MTU, adaptive retransmission on a lossy fabric. Budgeted at 8 hours; if it eats 20, the RDMA rows go and the TCP rows stand.</li>
	<li><strong>Node availability.</strong> 36 nodes of this type exist. Reserve the pair in week 1 for every measurement week.</li>
	<li><strong>Configuration traps already known.</strong> <code>dedup=on</code> means SHA-256; direct IO does nothing on zvols; the 100G interface stays down unless the profile declares a link on it.</li>
	<li><strong>Census realism.</strong> Scripted drift is not real drift. The fleet is built from real dated archives, the scripts are published, and the numbers it supplies are bounds the daemon is read against, not claims about fleets in the wild.</li>
</ul>

<h2>Logistics</h2>
<p>
	CS 4993, 1 credit.
	Expectations in writing before Sep 9.
	Thirty minutes of sponsor time every two weeks, with G1 as a scheduled meeting.
</p>

<h2>Future work</h2>
<p>
	<strong>Availability.</strong>
	The mirror arm is the seed of replication before ack; with it and k ≥ 2 on N ≥ 3 the system has a failure model, which needs membership, failure detection, and rebalancing, none of which this study touches.
</p>
<p>
	<strong>Placement.</strong>
	Super-chunk placement for locality, and a cache policy that weighs a chunk's owner distance.
</p>
<p>
	<strong>The same split elsewhere.</strong>
	Prefix caching in LLM serving (vLLM, SGLang, Mooncake) names cached KV blocks by a hash chain over the whole token history, so two requests share only along a common prefix; that is lineage.
	The same document after two different preambles is computed twice; that is the cross-host case here, and nobody has measured its size on a real trace.
</p>

<PageNav num="05" />
