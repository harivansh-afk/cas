<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="03" />
<p class="lede">
	<strong>Three stages.</strong> Each gates the next and ends with a standalone result.
</p>

<h2>S1 — Redundancy census (H1, weeks 1–4)</h2>
<p>
	Offline analysis of images at rest, requiring neither the daemon nor a guest.
	First numbers in two weeks.
</p>
<p>
	Corpora (A8 scripts for each): cloned fleet (golden image, N clones, scripted drift; lineage's
	best case). Convergent installs (N independent installs updated to the same package set;
	lineage's structural blind spot). Container layer stacks (cross-checks DupHunter at chunk
	granularity). Model family on the testbed (base, fine-tunes, quantizations; where whole-file
	dedup collapses). Nix store generations (successive closures of one flake; no published study).
</p>
<p>
	Method.
	Chunk every image at whole-file, CDC, and fixed granularities, and compute duplicate bytes.
	Against each corpus's declared ancestry, split them into lineage-capturable and cross-lineage.
	Lineage-capturable means identical and in-place relative to an ancestor, the ceiling for any
	COW system; a simulated COW at realistic record sizes and a declared snapshot cadence gives the
	practical figure, since sibling sharing depends on when snapshots were taken.
	The remainder is cross-lineage, reachable only by content.
	Compression in both orders and zeros excluded, per A7.
</p>
<p>
	The census settles standing claims as a side effect: whether compression captures most of
	dedup's win; whether fixed blocks still approximate CDC on VM images (the 2009 result,
	retested); whether whole-file dedup collapses on model corpora; and what fraction of observed
	sharing an explicit copy signal could ever have declared.
</p>

<h2>S2 — System comparison (H2, weeks 5–12)</h2>
<p>The four rungs on identical workloads, guest-visible metrics as the common denominator.</p>
<p>
	Workloads: fio (4K random write/read, 128K sequential, QD 1/8/32); kernel untar and build in the
	guest; N-clone boot storm; a read-heavy pass over settled data (the design's worst case, run
	young and aged); replay of the S1 fleet corpora.
</p>
<p>
	Measured per rung: guest p50/p99 write and read latency, compactor active and idle; write
	amplification (device bytes written per guest byte, from NVMe counters); storage consumed after
	ingest and after compaction settles; sustainable ingest ceiling and the back-pressure point;
	compaction bandwidth; index bytes per stored TB; recovery, <code>kill -9</code> then replay then
	<code>fio --verify</code>.
</p>
<p>
	Instrumentation: per-request stage timestamps inside the daemon, drained to ndjson,
	cross-checked once against bpftrace with the delta reported. ZFS is observed at the guest
	boundary plus <code>zpool</code> statistics; its internal stages are not comparable to the
	daemon's and the paper does not equate them. Controls: pinned vCPUs, performance governor,
	discarded warm-up, at least five repetitions, variance printed beside every number.
</p>

<h2>S3 — Distribution demonstration (H3, stretch)</h2>
<p>
	Two testbed nodes over their 100 GbE experiment link: placement by hash lands chunks on the
	correct owner; a fleet sync transfers bytes within G3's bound of unique bytes. Nothing further.
</p>

<h2>Gates</h2>
<ul class="reqs">
	<li>
		<span class="rid">G1</span>The census decomposition is exhaustive and disjoint; categories sum
		to 100% of non-zero bytes per corpus.
	</li>
	<li>
		<span class="rid">G2</span>The comparison table is complete: four rungs, identical workloads,
		latency, amplification, storage, and index columns, no empty cells.
	</li>
	<li>
		<span class="rid">G3</span>Two-node sync bytes within 10% of unique bytes.
	</li>
	<li>
		<span class="rid">G4</span>Recovery passes <code>fio --verify</code> after
		<code>kill -9</code> at arbitrary points, all rungs that involve the daemon.
	</li>
	<li>
		<span class="rid">G5</span>One command reruns every experiment on a second machine.
	</li>
</ul>

<h2>Schedule</h2>
<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Weeks</th><th>Stage</th><th>Result</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">1–2</td><td>S1</td><td>census pipeline; cloned-fleet and convergent-install splits</td></tr>
			<tr><td class="k">3–4</td><td>S1</td><td>remaining corpora; H1 verdict</td></tr>
			<tr><td class="k">5–6</td><td>S2</td><td>daemon skeleton over vhost-user; staging tier; R0 baseline runs</td></tr>
			<tr><td class="k">7–9</td><td>S2</td><td>compactor, store, offset map; R2 runs; R1 (ZFS) configured and run</td></tr>
			<tr><td class="k">10–11</td><td>S2</td><td>aged and read-heavy runs; debt ceiling; WA and interference</td></tr>
			<tr><td class="k">12</td><td>S2</td><td>Merkle-paged map; R3 runs; comparison table complete</td></tr>
			<tr><td class="k">13–14</td><td>—</td><td>report; reproducibility pack</td></tr>
			<tr><td class="k">stretch</td><td>S3</td><td>two-node placement and sync</td></tr>
		</tbody>
	</table>
</div>

<h2>Logistics and risks</h2>
<p>
	CS 4993, 1 credit for registration. Planned effort is roughly 8 hours weekly; the credit
	understates the work and this document does not. Expectations in writing before Sep 9; thirty
	minutes of sponsor time biweekly.
</p>
<p>
	Risks. Corpus bias is the principal threat to H1; A8 is the mitigation, and the corpus scripts
	are published so the classes themselves can be criticized. Daemon overrun is the principal
	threat to H2; the cut order is fixed in advance: R3 first, then the aging protocol, never the
	R0/R2 comparison or the census. The lineage-vs-content novelty claim was checked against the
	open web (2026-09-01) but not against OpenZFS development talks and mailing lists; those are
	swept before related work is final.
</p>

<PageNav num="03" />
