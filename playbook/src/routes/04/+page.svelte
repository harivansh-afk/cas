<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="04" />
<p class="lede">
	<mark>Three phases, each ending in something true.</mark>
	The census is the deliverable; the cost table is the second; the instrument is conditional on
	the first and labeled preliminary.
	Fourteen weeks at roughly eight hours each is about 110 hours, and the plan is sized to that
	number rather than to the ambition.
</p>

<h2>Hardware</h2>
<p>
	The study runs on x86-64 bare metal, the architecture of every system in the comparison
	literature, so results compare directly to prior work.
	Phase 1 needs one node and no root; phases 2 and 3 need one node with a dedicated NVMe device.
</p>
<p>
	Primary testbed: <a href="https://docs.cloudlab.us/hardware.html">CloudLab c6525-100g</a>
	nodes (Utah cluster), one at a time.
	Per node: one AMD EPYC 7402P, 24 cores at 2.80 GHz, Zen 2; 128 GB ECC DDR4-3200; two 1.6 TB
	NVMe SSDs, PCIe 4.0; one 25 GbE and one 100 GbE experiment link.
	One NVMe device holds the system and results; the second is dedicated to the store under test,
	so guest IO and any dedup pass never share a device with the OS.
	CloudLab allocations are free for sponsored academic research; the sponsor approves the project.
</p>
<p>
	Fallback if CloudLab access is not granted: one
	<a href="https://corporate.ovhcloud.com/en/newsroom/news/adv-gen3-announcement/">OVHcloud
	Advance</a> bare-metal server (2026 line), AMD EPYC 4005-series, 16 cores/32 threads, DDR5
	ECC, 2 × 960 GB NVMe.
</p>
<p>
	The CloudLab NICs are ConnectX-5 and therefore RoCE-capable; no phase uses the network for data.
	Neither testbed has persistent memory or accelerators; the commodity restriction is part of the
	claim.
	Every figure in the paper is measured on the testbed rather than quoted.
</p>

<h2>Schedule</h2>
<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Weeks</th><th>Phase</th><th>Result</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">1–2</td><td>1</td><td>census pipeline; synthetic controls run; first real-fleet ask sent</td></tr>
			<tr><td class="k">3–4</td><td>1</td><td>first real fleet hashed on site; model and Nix corpora; first curves</td></tr>
			<tr><td class="k">5–6</td><td>1</td><td>second real fleet; curves final; H1 verdict; <strong>week-6 gate written down</strong> (G2, G4)</td></tr>
			<tr><td class="k">7–8</td><td>2</td><td>R0 and R1 configured; fio, kernel build, boot storm</td></tr>
			<tr><td class="k">9–10</td><td>2</td><td>R2 dm-vdo and R3 duperemove; fleet replay; cost table complete (G3)</td></tr>
			<tr><td class="k">11–12</td><td>3 or 1</td><td>gate open: passthrough, staging, compactor, verify, R4 row · gate closed: third fleet, tighter curves</td></tr>
			<tr><td class="k">13–14</td><td>—</td><td>report; reproducibility pack (G5)</td></tr>
		</tbody>
	</table>
</div>

<h2>Gates</h2>
<ul class="reqs">
	<li>
		<span class="rid">G1</span>The census decomposition is exhaustive and disjoint: leaves sum to
		100% of non-zero bytes per corpus at every time point.
	</li>
	<li>
		<span class="rid">G2</span>At least two real fleets in the census, hashed on site under the
		published donor protocol.
	</li>
	<li>
		<span class="rid">G3</span>The cost table is complete: four stock rungs, identical workloads,
		latency, amplification, storage, index, transfer, and cache columns, no empty cells, variance
		beside every number.
	</li>
	<li>
		<span class="rid">G4</span>The phase-3 decision is written at the end of week 6 with its
		threshold (block-capturable at 4K ≥ 90% of cross-lineage on VM corpora cancels it) and cannot
		move afterward. If phase 3 runs, <code>kill -9</code> recovery passes
		<code>fio --verify</code> before any R4 number is reported.
	</li>
	<li>
		<span class="rid">G5</span>One command reruns the census on any directory of images with an
		ancestry file; one command reruns the cost table on a second node.
	</li>
</ul>

<h2>Cut order, fixed now</h2>
<p>
	Phase 3 first.
	Then the Nix corpus.
	Then R2 and R3, keeping R0 and R1.
	Never a real fleet, never the time axis, never the R0/R1 table.
	The order is the order the hours are spent, and a slip removes items from the top of this list,
	not from the bottom.
</p>

<h2>Logistics</h2>
<p>
	CS 4993, 1 credit for registration. Planned effort is roughly 8 hours weekly; the credit
	understates the work and this document does not. Expectations in writing before Sep 9; thirty
	minutes of sponsor time biweekly, with the week-6 gate as a scheduled meeting.
</p>

<h2>Risks</h2>
<p>
	<strong>No donor.</strong>
	The principal threat to the study. Two real fleets is a gate, so the ask goes out in week 1 to
	several candidates at once, the protocol moves hashes rather than images, and the author's own
	machines are the floor.
	If only one real fleet lands, the paper reports one and says so; if none, the census stands on
	synthetic controls and the paper's claims shrink to the classes it measured.
</p>
<p>
	<strong>Corpus bias.</strong>
	A8 is the mitigation; corpus scripts and the donor protocol are published so the classes
	themselves can be criticized, and the synthetic controls bracket every real curve.
</p>
<p>
	<strong>Phase-3 overrun.</strong>
	Contained by construction: it runs only if the gate opens, it is two weeks, its numbers are
	labeled preliminary, and it is first in the cut order.
</p>
<p>
	<strong>Novelty.</strong>
	The lineage-versus-content claim was checked against the open web on 2026-09-01; that sweep
	added DeDe, Jayaram et al., El-Shimi et al., TiDedup, and HYDRAstor to related work and found no
	split measurement. OpenZFS development talks and mailing lists are not yet swept; they are,
	before related work is final.
</p>

<h2>What comes out</h2>
<p>
	A measurement paper with a decision rule an operator can apply to their own fleet using the
	published pipeline.
	A four-backend cost table on identical hardware with numbers nobody has published side by side.
	A data-backed answer to whether content-defined chunking on a guest block path deserves a
	system, and if it does, a scoped instrument and preliminary numbers for the next study.
	The shape is a strong undergraduate thesis and a credible workshop submission; it is not a
	half-built daemon beside a rushed census, which is what the same hours spent the other way
	around would produce.
</p>

<PageNav num="04" />
