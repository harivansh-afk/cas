<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="00" />
<p class="lede">
	<strong>casblk measures the latency cost of content-addressed storage on the guest disk path.</strong>
	The system is the instrument. The paper is the product.
</p>

<h2>Claims</h2>
<p>The paper makes three claims. Every design decision serves one of them.</p>
<ul class="reqs">
	<li><span class="rid">C1</span><strong>Taxonomy.</strong> A per-stage latency breakdown of a virtio-blk request over a content-addressed backend. The taxonomy attributes ≥ 90% of the p99 gap between the raw-file backend and the CAS backend.</li>
	<li><span class="rid">C2</span><strong>Tradeoff curve.</strong> Dedup ratio against tail latency, across chunk sizes and hash functions.</li>
	<li><span class="rid">C3</span><strong>Async-hash result.</strong> The latency recovered when the hash moves off the critical path, and the size of the integrity window this opens.</li>
</ul>

<h2>Functional bar</h2>
<p>The instrument is trustworthy when four checks pass. Nothing more is required.</p>
<ul class="reqs">
	<li><span class="rid">F1</span>The backend boots a stock Linux guest.</li>
	<li><span class="rid">F2</span>The backend implements the five virtio-blk request types.</li>
	<li><span class="rid">F3</span><code>fio --verify</code> passes on every arm.</li>
	<li><span class="rid">F4</span>The store recovers after <code>kill -9</code>: rescan the log, rebuild the index, pass verify.</li>
</ul>

<h2>Out of scope</h2>
<p>GC during benchmarks. Live migration. Multi-host operation. Security hardening. Snapshot trees (one COW copy only). qcow2 inside the VMM.</p>

<h2>Novelty (checked 2026-08-31)</h2>
<p>Dedup latency studies measure the backend on bare metal (iDedup FAST'12, Dmdedup OLS'14, VDO TOS'24). Virtio studies measure the transport over plain backends (Spool ATC'20, LightIOV). No published work measures the intersection per stage. Closest: CLB (VEE'17) uses content addressing as an optimization; it does not measure its cost.</p>

<h2>Context</h2>
<p>CS 4993, 1 credit, ~3 h/week, 14 weeks. Sponsor: Cai (latency framing) or Cheng (dedup framing). Expectations in writing before Sep 9.</p>

<PageNav num="00" />
