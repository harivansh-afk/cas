<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="03" />
<p class="lede">
	Three stages. Each stage gates the next, ends with a standalone result, and maps to one
	hypothesis.
</p>

<h2>S1 — The redundancy census (H1, weeks 1–4)</h2>
<p>Offline analysis of images at rest. No VMM, no store, no root; first numbers in two weeks.</p>
<p>
	Corpora, each with its generative story: a cloned fleet (one golden image, N clones, scripted
	drift; lineage's best case); convergent installs (N independent installs updated to the same
	package set; lineage's blind spot); container layer stacks (cross-checks DupHunter at chunk
	level); a model family on the Spark (base, fine-tunes, quantizations; where whole-file dedup
	collapses); Nix store generations (successive closures of one flake; studied by nobody).
</p>
<p>
	Method: chunk every image at whole-file, CDC, and fixed granularities. Compute total duplicate
	bytes; then, against each corpus's declared ancestry, split them into lineage-capturable
	(identical and in-place relative to an ancestor, the upper bound any COW system reaches; plus a
	simulated COW at realistic record sizes) and cross-lineage (the remainder only content
	addressing finds). Compression measured in both orders per A7; zeros excluded per A7.
</p>
<p>
	The census also settles standing folk claims with data: whether compression captures most of
	dedup's win, whether fixed blocks still roughly match CDC on VM images (the 2009 result,
	retested), whether whole-file dedup collapses on model corpora, and what fraction of sharing an
	explicit &ldquo;copy me&rdquo; signal could ever have declared.
</p>

<h2>S2 — The system comparison (H2, weeks 5–12)</h2>
<p>
	The three-rung ladder on identical workloads, on the testbed, guest-visible metrics as the
	common denominator.
</p>
<p>
	Workloads: fio microbenchmarks (4K random write and read, 128K sequential, queue depths 1/8/32),
	a kernel untar-and-build inside the guest, an N-clone boot storm, and replay of the S1 fleet
	corpora through each system.
</p>
<p>
	Measured, per rung: guest-visible write and read latency (p50/p99), young and aged, with a
	scripted aging protocol of overwrite and discard cycles; storage consumed after ingest and after
	compaction settles; sustained ingest ceiling before chunking debt grows without bound, and where
	back-pressure lands; compaction bandwidth; index bytes per stored TB as measured constants in
	the extrapolation formula; recovery: <code>kill -9</code>, rescan, <code>fio --verify</code>
	clean.
</p>
<p>
	Instrumentation: per-request stage timestamps inside the backend, drained to ndjson,
	cross-checked once against bpftrace; ZFS observed at the same guest-visible boundary plus
	<code>zpool</code> statistics, because inside-ZFS stages are not comparable and the paper does
	not pretend otherwise. Controls: pinned vCPUs, performance governor, discarded warm-up, five or
	more repetitions, variance printed beside every number.
</p>

<h2>S3 — The distribution demonstration (H3, stretch)</h2>
<p>
	Two nodes on the existing link. Show placement lands chunks by hash, and a fleet sync transfers
	bytes proportional to unique bytes. Deliberately small; the page-02 design carries the argument,
	and the follow-on study carries the benchmark.
</p>

<h2>Gates</h2>
<ul class="reqs">
	<li>
		<span class="rid">G1</span>The census decomposition is exhaustive and disjoint: categories sum
		to 100% of non-zero bytes, per corpus.
	</li>
	<li>
		<span class="rid">G2</span>The comparison table is complete: three systems, same corpora,
		guest-visible latency plus storage cost plus index cost, no empty cells.
	</li>
	<li>
		<span class="rid">G3</span>Sync bytes within 10% of unique bytes on the two-node
		demonstration.
	</li>
	<li><span class="rid">G4</span>One command reruns everything on a second machine.</li>
</ul>

<h2>Schedule and logistics</h2>
<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Weeks</th><th>Stage</th><th>Result</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">1–2</td><td>S1</td><td>census pipeline; cloned-fleet and convergent-install splits</td></tr>
			<tr><td class="k">3–4</td><td>S1</td><td>remaining corpora; H1 verdict; folk-claim table</td></tr>
			<tr><td class="k">5–8</td><td>S2</td><td>staging tier + compactor + offset-tree map; ZFS rung configured</td></tr>
			<tr><td class="k">9–12</td><td>S2</td><td>prolly rung; aged runs; debt ceiling; full comparison table</td></tr>
			<tr><td class="k">13–14</td><td>—</td><td>report and reproducibility pack</td></tr>
			<tr><td class="k">stretch</td><td>S3</td><td>two-node placement and sync demonstration</td></tr>
		</tbody>
	</table>
</div>
<p>
	CS 4993, 1 credit, about 3 hours weekly. Expectations in writing before Sep 9. Thirty minutes of
	sponsor time every other week. Risks, named: corpus bias is the main threat to H1, mitigated by
	A8; the S2 build overrunning is the main threat to H2, mitigated by cutting the prolly rung
	before cutting the ZFS comparison; the lineage-vs-content novelty claim was checked against the
	open web (2026-09-01) but not against OpenZFS dev-summit talks and mailing lists, which get
	swept before related work is final.
</p>

<PageNav num="03" />
