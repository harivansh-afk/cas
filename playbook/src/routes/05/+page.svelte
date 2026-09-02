<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="05" />
<p class="lede">
	<strong>Fourteen weeks, about 320 hours.</strong><br />
	That is 23 a week.<br />
	The course credit corresponds to 8.<br />
	The plan is sized to the work, and the descoping order defines what is removed if it slips.
</p>

<h2>Hardware</h2>
<p>
	Two CloudLab c6525-100g nodes (Utah), reserved as a pair.<br />
	Per node: AMD EPYC 7402P, 24 cores at 2.80 GHz; 128 GB ECC DDR4-3200; two 1.6 TB PCIe 4.0 NVMe SSDs; ConnectX-5 Ex 100 GbE, one port on the experiment network.<br />
	One NVMe holds the system and results; the other is the device under test.<br />
	The pair is one hop through a single switch.
</p>
<p>
	RoCE between two of these nodes works and has been used in published work on this exact hardware, on a lossy fabric.<br />
	Self-built kernels are routine there; the Ubuntu 24.04 image ships 6.8, dm-vdo needs 6.9, and OpenZFS 2.3 is a source build, so a kernel and ZFS are built once in week 1 and snapshotted as an image.<br />
	Reservations expire at 16 hours by default, so every run is scripted to complete inside one.
</p>
<p>
	CloudLab is free for research.<br />
	A project is opened by a faculty member and reviewed by CloudLab staff; the sponsor opens it before Sep 9.<br />
	Fallback: two OVHcloud Advance-4 2026 servers (EPYC 4585PX, 16 cores, 64 GB DDR5 ECC, 2 × 960 GB NVMe) on a 25 Gbps private link, which loses the RDMA arm and replaces the 100 GbE fabric with 25 GbE.
</p>

<h2>Schedule</h2>
<div class="table-scroll">
	<table class="spec prose">
		<thead>
			<tr><th>Weeks</th><th>Build</th><th>Measure</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">1–2</td><td>vhost-user-blk daemon in passthrough: staging log, FLUSH, replay. Kernel and ZFS image.</td><td>R0; passthrough within 10% of R0 p99 (G1). Thresholds frozen. <code>zdb -S</code> phase 0 on the synthetic fleet.</td></tr>
			<tr><td class="k">3–5</td><td>Compactor with settle window, store, index, maps, watermark, governor, recovery. Three chunk-size arms.</td><td><code>kill -9</code> recovery and the three ordering tests pass (G2). First capture numbers.</td></tr>
			<tr><td class="k">6–7</td><td>R1 configured, both volblocksize arms. R2 if time permits.</td><td>Part 1 table complete (G3), sweep before every capacity number.</td></tr>
			<tr><td class="k">8–9</td><td>Protocol with separate GET and PUT connections, rendezvous placement, k, segment PUT with durable ack, HAS, pins, sweep. Provisioning; migration with the fenced handoff.</td><td>Replicated mode on two nodes.</td></tr>
			<tr><td class="k">10</td><td>Partitioned mode. Fleet class over TCP.</td><td>Part 2 table complete (G4).</td></tr>
			<tr><td class="k">11–12</td><td>nvmet exports, RoCE configuration, busy-polling and blocking daemon, depth prefetch, profile prefetch.</td><td>Transport matrix and prefetch sweeps (G5). Partitioned boot storm.</td></tr>
			<tr><td class="k">13–14</td><td></td><td>Report; reproducibility pack (G6).</td></tr>
		</tbody>
	</table>
</div>

<h2>Gates</h2>
<ul class="reqs">
	<li><span class="rid">G1</span>Passthrough daemon under stock QEMU within 10% of R0 p99 by the end of week 2. If this slips, everything after it slips, and the sponsor is informed that week.</li>
	<li><span class="rid">G2</span><code>kill -9</code> at arbitrary points, replay, <code>fio --verify</code> passes, before any daemon number is reported. Three ordering tests pass with it: a FLUSH racing writes on another queue, a discard of an unwritten range, and a stalled daemon that is restarted with the guest still recoverable.</li>
	<li><span class="rid">G3</span>Part 1 table complete: R0, R1 at two block sizes, R3 at three chunk sizes; latency, capture, index, amplification; variance beside every number.</li>
	<li><span class="rid">G4</span>Part 2 table complete: both modes, every flow, bytes transferred against the census bound.</li>
	<li><span class="rid">G5</span>Transport matrix complete for every non-stretch probe, null and file, memory and NVMe, with RoCE counters at zero.</li>
	<li><span class="rid">G6</span>One command rebuilds the fleet from dated archives; one command reruns every table on a fresh pair.</li>
</ul>

<h2>Descoping order</h2>
<p>
	When the schedule slips, items come off from the top.
</p>
<ol class="steps">
	<li>ibverbs daemon arm, and with it fleet class over RDMA.</li>
	<li>Super-chunk placement.</li>
	<li>R2 dm-vdo.</li>
	<li>Profile prefetch (depth prefetch stays).</li>
	<li>Fleet class over TCP. H4 then stands on the literature's numbers and says so.</li>
	<li>Partitioned mode. Replicated mode alone still gives H2's transfer result.</li>
	</ol>
<p>
	Not removed under any slip: part 1, the nvmet TCP and RDMA probes, and the daemon over TCP.
</p>

<h2>Risks</h2>
<ul class="plain">
	<li><strong>Daemon overrun.</strong> The largest risk and the reason G1 is at week 2. Protocol plumbing comes from maintained crates so the hours go to the components listed as new code on page 01.</li>
	<li><strong>RoCE configuration.</strong> GID selection, MTU, adaptive retransmission on a lossy fabric. Budgeted at 8 hours; if it exceeds 20, the RDMA rows are dropped and the TCP rows stand.</li>
	<li><strong>Node availability.</strong> 36 nodes of this type exist. Reserve the pair in week 1 for every measurement week.</li>
	<li><strong>Correctness debt.</strong> The bugs that stall a guest are known in advance: a FLUSH that misses a write on another queue, a discard that acknowledges a sequence number nothing wrote, a daemon that stops and leaves the guest in D-state. Each has a test in G2 and hours in weeks 3 to 5, before any number is taken.</li>
	<li><strong>Known configuration pitfalls.</strong> <code>dedup=on</code> means SHA-256; direct IO does nothing on zvols; the 100G interface stays down unless the profile declares a link on it.</li>
	<li><strong>Census realism.</strong> Scripted drift is not real drift. The fleet is built from real dated archives, the scripts are published, and the numbers it supplies are bounds the daemon is read against, not claims about fleets in the wild.</li>
</ul>

<h2>Logistics</h2>
<p>
	CS 4993, 1 credit.<br />
	Expectations in writing before Sep 9.<br />
	Thirty minutes of sponsor time every two weeks, with G1 as a scheduled meeting.
</p>

<h2>Future work</h2>
<p>
	<strong>Availability.</strong><br />
	Fleet class is the seed of replication before ack; with it and k ≥ 2 on N ≥ 3 the system has a failure model, which needs membership, failure detection, and rebalancing, none of which this study touches.
</p>
<p>
	<strong>Placement.</strong><br />
	Super-chunk placement for locality, and a cache policy that weighs a chunk's owner distance.
</p>
<p>
	<strong>The same split elsewhere.</strong><br />
	Prefix caching in LLM serving (vLLM, SGLang, Mooncake) names cached KV blocks by a hash chain over the whole token history, so two requests share only along a common prefix; that is lineage.<br />
	The same document after two different preambles is computed twice; that is the cross-host case here, and nobody has measured its size on a real trace.
</p>

<PageNav num="05" />
