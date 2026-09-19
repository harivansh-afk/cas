<script lang="ts">
	import { base } from '$app/paths';
	import '$lib/architecture/article.css';
	import Integration from '$lib/architecture/Integration.svelte';
	import { sourceAt } from '$lib/architecture/source';
	import status from '$lib/architecture/integration.json';

	const rev = status.revisions;
	const at = sourceAt(rev.main_full);
	const doc = (name: string) => at(`docs/${name}.md`);
	const pr = (n: number) => `https://git.harivan.sh/harivansh-afk/cas-research/pulls/${n}`;
	const review = status.review;
	const sections = [
		['status', 'Where the study stands'], ['review', 'One pass over every file'],
		['acceptance', 'The fix, measured against a control'], ['risks', 'What the numbers do not say'],
		['next', 'Next'], ['evidence', 'Evidence and revisions']
	];
	const lines = (d: { insertions: number; deletions: number }) => `+${d.insertions.toLocaleString('en-US')} / −${d.deletions.toLocaleString('en-US')}`;
	const todoOpen = Object.entries(status.todo.sections).filter(([, s]) => s.open > 0);
</script>

<svelte:head>
	<title>Update 03 · Reads no longer wait on writers</title>
	<meta name="description" content="Status of the CAS study on 15 September 2026: a full-codebase review, two runtime fixes measured against a same-session control, where checkpoints and research gates stand, and what comes next." />
</svelte:head>

<article class="architecture" id="beginning">
	<header>
		<a class="back" href="{base}/">← index</a>
		<span class="eyebrow">Update 03 · 15 September 2026</span>
		<h1>Reads no longer wait on writers</h1>
		<p class="lede">The single-host backend is functionally complete and now reviewed end to end. This update reports that review, the two runtime defects it found, and a live measurement of the fix against the previous scheduler under identical conditions. It then says plainly how far the research study itself has progressed: not yet past its first gate.</p>
		<p><a href="{base}/updates/2/">Update 02</a> explains how the backend works. Nothing in the architecture changed here; this page is about its state.</p>
		<p class="checkpoint"><strong>Latest finding:</strong> with the previous scheduler, independent reads waited up to {(Math.max(...status.lab.final.control.read_admission_max_wait_ms) / 1000).toFixed(1)} s for admission while two images retried blocked writes. After the fix the same counter peaked at {Math.max(...status.lab.final.off.read_admission_max_wait_ms).toFixed(0)} ms, same-CPU read p99 fell from hundreds of milliseconds to 9–20 ms, and writer throughput did not change. Multi-second tails from FLUSH barriers behind WAL-blocked writes remain. <a href="#acceptance">Measurements ↓</a></p>
	</header>

	<nav aria-label="Update contents"><ol>{#each sections as [id, title]}<li><a href={`#${id}`}>{title}</a></li>{/each}</ol></nav>

	<section id="status">
		<h2>Where the study stands</h2>
		<p>The implementation checkpoints C0–C5 describe the local backend; the research gates G1–G6 describe the study. The first column is done in development form. The second has not started, because it needs dedicated hardware and a ZFS comparator that do not yet exist.</p>
		<div class="table-scroll"><table class="spec"><thead><tr><th>Checkpoint</th><th>State</th><th>Open</th></tr></thead><tbody>
			{#each status.checkpoints as c}<tr><td>{c.id}</td><td>{c.state}{#if 'scenarios' in c}: {c.scenarios} scenarios{/if}</td><td>{'open' in c ? c.open : c.note}</td></tr>{/each}
		</tbody></table></div>
		<div class="table-scroll"><table class="spec"><thead><tr><th>Gate</th><th>What it requires</th><th>State</th></tr></thead><tbody>
			{#each status.gates as g}<tr><td>{g.id}</td><td>{g.name}</td><td>{g.state}</td></tr>{/each}
		</tbody></table></div>
		<p>The progress tracker has {status.todo.done} items checked and {status.todo.open} open: {#each todoOpen as [name, s], i}{name} {s.open}{i < todoOpen.length - 1 ? ', ' : ''}{/each}. Native checks on current main pass {status.tests.passed} tests with {status.tests.ignored} fixture-dependent tests ignored. The consolidated C5 suite last ran on <code>{rev.c5}</code>; the scheduler and compactor have changed since, and it has not been rerun.</p>
	</section>

	<section id="review">
		<h2>One pass over every file</h2>
		<p>Six independent reviewers each read a slice of the roughly {(review.first_party_lines / 1000).toFixed(0)}k lines of first-party Rust, every file in full. They agreed on the shape. <strong>The invariants are carried by types</strong>: aligned buffers, budgeted allocation that charges before it allocates, permits and tickets that release on drop, receipts only successful IO can produce, formats validated on decode and round-tripped on encode. Every unsafe block has a justification that holds. No data-loss, ordering or lock-order bug was found in the storage, replay, carrier or cache paths.</p>
		<p><strong>The seams are overgrown.</strong> The vhost backend is a 34-field struct spread over four files sharing private state; a storage enum’s variant checks leak into it; three state machines run 200 lines or more mixing events with policy; the same small helpers were copied three to nine times; reports were untyped JSON beside a typed pattern; and four different things are called “admission”.</p>
		<h3>What changed</h3>
		<div class="table-scroll"><table class="spec"><thead><tr><th>PR</th><th>Kind</th><th>Change</th></tr></thead><tbody>
			{#each review.prs as p}<tr><td><a href={pr(p.number)}>#{p.number}</a></td><td>{p.kind}</td><td>{p.title}</td></tr>{/each}
		</tbody></table></div>
		<p>All six merged through <a href={pr(review.integration_pr)}>#{review.integration_pr}</a> as {review.commits} commits: {lines(review.diff.core)} lines in <code>cas-core</code>, {lines(review.diff.daemon)} in <code>cas-daemon</code>, {lines(review.diff.harness)} in the harness, CLI and Nix. Each refactor commit is behaviour-preserving; report JSON was checked byte for byte against the old output, and CLI help text is unchanged.</p>
		<details><summary>The two runtime defects</summary>
			<p><strong>Read starvation with two images.</strong> A refused write’s retry marked itself ready again and, as the oldest head, was chosen ahead of the eligible read behind it. Each refusal handed the turn to the other image, which repeated the pattern, so neither read ran until write capacity returned. The refused head now rejoins behind its image’s other heads. The recorded reproduction that failed after 64 visits passes within four; retry self-healing is unchanged, so no write can strand.</p>
			<p><strong>A denied attachment lost the image.</strong> Attaching an image took it out of its slot before cloning descriptors, charging the metadata budget and binding its wakes. A denial in any of those left the image permanently unattachable and its admission wake marked attached, which would fail the next host quiescence. Fallible steps now run first; the regression test fails on the old ordering at the stale-wake assertion.</p>
		</details>
		<details><summary>Design decisions left open on purpose</summary>
			<ul class="findings">
				<li><strong>Storage dispatch.</strong> Give the storage enum one submission entry point so the backend stops inspecting its variant.</li>
				<li><strong>Background deadline.</strong> The scheduler’s fixed 30 s background wait can mark the chunk store failed when a demand reactor stalls. Make it a parameter or a retryable refusal before the first byte.</li>
				<li><strong>Page-cache key.</strong> It includes the manifest end offset, so every new publication misses on unchanged interior pages.</li>
				<li><strong>Permit ordering.</strong> Forgetting one call on a staging permit fails the whole account; a typestate would make it a compile error.</li>
				<li><strong>Two staging logs.</strong> The v1 log is still used by the older daemon path and the CLI beside the v2 write-ahead log.</li>
				<li><strong>Unused accounting.</strong> Three disk-reservation methods have no production caller and are now test-only; delete or wire them in.</li>
			</ul>
			<p><a href={doc('validation/2026-09-14-fair-retry')}>Scheduler fix record</a> · <a href={doc('validation/2026-09-14-attach-failure')}>Attach fix record</a> · <a href={doc('validation/2026-09-15-cleanup-integration')}>Integration record</a></p>
		</details>
	</section>

	<section id="acceptance">
		<h2>The fix, measured against a control</h2>
		<p>The integrated source passed its native checks and all {status.lab.smoke_cases} live QEMU recovery and reset cases. Then the same two-guest mixed workload from Update 02 ran three times in one session: first on the previous runtime as a control, then twice on the new one.</p>
		<Integration />
		<p><strong>What did not change.</strong> Slowest reads of 5–25 s appear in every arm, including the control. The same-CPU ones coincide with ordinary-head waits of the same length, consistent with writes and FLUSH barriers queued behind WAL capacity while compaction drained at about half its earlier rate on the loaded host; the separate-CPU ones occur with almost no bypasses and are a different mechanism. The second pass had no same-CPU read above 0.7 s. These tails are the next scheduling item.</p>
	</section>

	<section id="risks">
		<h2>What the numbers do not say</h2>
		<ul class="findings">
			<li><strong>Nothing here is a research result.</strong> Every number in this repository comes from TCG guests nested in a KVM VM on one shared workstation. G1 needs a dedicated host and a raw baseline on real media; neither exists.</li>
			<li><strong>Performance is far from the hypothesis thresholds.</strong> The only CAS-versus-raw comparison, on 13 September, put fdatasync p99 at {status.baseline_2026_09_13.fdatasync_p99_ms.cas} ms against {status.baseline_2026_09_13.fdatasync_p99_ms.raw} ms and sequential reads at {status.baseline_2026_09_13.sequential_read_mib_s.cas} against {status.baseline_2026_09_13.sequential_read_mib_s.raw} MiB/s. The study allows 20% on p99. Whether that gap is emulation or design is unknown until native media is measured.</li>
			<li><strong>Read isolation is better, not solved.</strong> The starvation loop is gone; the FLUSH-barrier and separate-CPU tails are not. Any latency claim is meaningless with multi-second outliers.</li>
			<li><strong>The consolidated suite is stale.</strong> C5 passed on <code>{rev.c5}</code>. Four scheduler and compactor changes have merged since, each with its own focused checks but no full rerun.</li>
			<li><strong>Process weight.</strong> Each result is written four times, the design notes are unindexed, the validation log is out of order, and about 60 merged worktrees still hold cited evidence. The records are honest; their volume is consuming velocity the gates need.</li>
		</ul>
	</section>

	<section id="next">
		<h2>Next</h2>
		<ol>
			<li>Bound the remaining tails: FLUSH barriers behind WAL-blocked writes and separate-CPU maxima, with a traced repeat of this workload.</li>
			<li>Rerun the consolidated C5 suite on current main and close the C4/C5 allocation audit.</li>
			<li>Secure two dedicated hosts and experiment disks; measure raw XFS and passthrough latency for G1; boot the pinned ZFS profile for G3.</li>
			<li>Decide the open design items above; archive merged worktrees after moving their cited evidence; index the design notes.</li>
		</ol>
	</section>

	<section id="evidence">
		<h2>Evidence and revisions</h2>
		<div class="table-scroll"><table class="spec"><thead><tr><th>Record</th><th>What it establishes</th></tr></thead><tbody>
			<tr><td><a href={doc('validation/2026-09-15-cleanup-integration')}>15 September integration</a></td><td>{status.tests.passed} native tests, {status.lab.smoke_cases} live recovery/reset cases and three mixed-IO arms with a control on <code>{rev.integration}</code>, merged as <code>{rev.main}</code>.</td></tr>
			<tr><td><a href={doc('measurements/integration-2026-09-15/README')}>Comparison tables</a></td><td>Per-stage fio and telemetry for control and both integration passes; the script that produced them.</td></tr>
			<tr><td><a href={doc('validation/2026-09-14-read-progress-live')}>14 September read scheduler</a></td><td>Bypass working on <code>{rev.update02}</code>; the two-image starvation reproduced natively.</td></tr>
			<tr><td><a href={doc('validation/2026-09-13-c5-final')}>13 September C5</a></td><td>41 of 41 scenarios on <code>{rev.c5}</code>, independently verified.</td></tr>
		</tbody></table></div>
		<p>Links point at <a href={at('')}>{rev.main}</a>, current main. Raw lab artifacts and receipts stay on Spark under the integration worktree; bulk VM data was removed after verification per the <a href={doc('artifact-retention')}>retention policy</a>. <a href={at('TODO.md')}>Progress tracker</a> · <a href={doc('validation')}>Validation history</a>.</p>
	</section>

	<footer><a href="{base}/">← index</a><a href="#beginning">↑ beginning</a><a href="{base}/updates/2/">Update 02 ↗</a></footer>
</article>
