<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="05" />

<h2>Tree</h2>
<pre>casblk/
  crates/
    chunkstore/    # log, index, BLAKE3, hole-punch reclaim; no virtio deps
    blockmap/      # flat map + journal, COW snapshot
    backend/       # backend trait: raw-file impl, cas impl
    trace/         # T0–T7 ring buffer → ndjson
  vmm/             # thin glue into the existing rust-vmm VMM
  harness/
    workloads/     # fio jobfiles, kernel-build script, boot-time
    corpus/        # distro image fetch scripts
    runner/        # one command per arm; tagged ndjson out
    analyze/       # uv-run python: taxonomy plots, tradeoff curve
  results/         # committed ndjson + figures per tagged run
  docs/            # design.md, methodology.md</pre>

<h2>Milestones</h2>
<div class="table-scroll">
	<table class="spec">
		<thead><tr><th>Weeks</th><th>Work</th><th>Gate</th></tr></thead>
		<tbody>
			<tr><td class="k">1–2</td><td>Harness + raw-file baseline. Preliminary numbers for the pitch.</td><td>F1, first p99s</td></tr>
			<tr><td class="k">3–5</td><td>CAS backend wired and instrumented. First taxonomy.</td><td>F2, F3, C1 draft</td></tr>
			<tr><td class="k">6–9</td><td>Sweep chunk, hash, index arms. Build the curve.</td><td>V1, V2</td></tr>
			<tr><td class="k">10–12</td><td>Async-hash experiment.</td><td>V3</td></tr>
			<tr><td class="k">13–14</td><td>Report + reproducibility pack.</td><td>V4, V5</td></tr>
		</tbody>
	</table>
</div>

<h2>Stretch goals</h2>
<p>Discard-driven mark-and-sweep GC with hole punch (CAS-18). Prolly-tree block map for delta-proportional image sync. Verify-on-dedup arm (CAS-15).</p>

<h2>Risks</h2>
<p>Integration overrun in weeks 3–5 is the main risk. Fallback: a fixed-4K-only CAS backend keeps C1 and C3 and drops part of C2's curve. Agree to this fallback with the sponsor in writing.</p>

<h2>Distribution note</h2>
<p>The data plane distributes later with a CRUSH-style placement function over the hash; chunks are immutable and location-independent. The mutable pieces (maps, liveness) and remote-read p99 are the follow-on project, not this one.</p>

<PageNav num="05" />
