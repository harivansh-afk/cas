<script lang="ts">
	import data from './read-pressure.json';
	const reportCommit = '02fa6e8eb9b74d3e91d7a4f360adda4fd44485b0';
	const record = (path: string) => `https://git.harivan.sh/harivansh-afk/cas-research/src/commit/${reportCommit}/${path}`;
	const source = (path: string) => `https://git.harivan.sh/harivansh-afk/cas-research/src/commit/${data.source_revision}/${path}`;
	const examples = data.rows.filter(row => row.stage !== 'baseline' && row.slowest_retained_seed_read);
	let selected = $state('same-1-2');
	const row = $derived(examples.find(row => `${row.stage}-${row.guest}` === selected)!);
	const trace = $derived(row.slowest_retained_seed_read!);
	const phases = $derived(row.slowest_phases_ns!);
	const post = $derived(trace.finished_ns - trace.admitted_ns);
	const percent = $derived(trace.behind_write_ns / trace.finished_ns * 100);
	const ms = (ns: number) => (ns / 1e6).toFixed(3);
	const labels: Record<string, string> = {
		'same-1': 'Same queue · pass 1', 'same-2': 'Same queue · pass 2',
		'separate-1': 'Separate CPUs · pass 1', 'separate-2': 'Separate CPUs · pass 2'
	};
	const execution = $derived(phases.execution_breakdown);
</script>

<div class="read-pressure">
	<p class="finding"><strong>A cached read waited 7.922 seconds behind writes.</strong> Once admitted, it finished in 1.34 ms. The long wait came before the read entered storage. <a href={record('docs/measurements/read-pressure-trace-2026-09-14/README.md')}>14 September instrumented probe</a>.</p>
	<div class="queue" aria-label="A write waiting for WAL capacity prevents later reads on this queue from reaching admission">
		<span class="queue-label">Guest ring</span><span class="blocked">Write waiting<br /><small>WAL capacity</small></span><span class="arrow" aria-hidden="true">←</span><span class="waiting">Read</span><span class="waiting">Read</span>
	</div>
	<p class="explanation">The frontend stops consuming this virtqueue at its blocked write. The queue mutex is released; the requests remain ordered behind that head. Measured queue-lock acquisition peaked at 0.187 ms. Making the queue lock-free would not change this admission rule.</p>

	<label for="read-example">Follow one measured 4 KiB seed read</label>
	<select id="read-example" bind:value={selected}>
		{#each examples as example}
			<option value={`${example.stage}-${example.guest}`}>{labels[example.stage]} · guest {example.guest}</option>
		{/each}
	</select>
	<div class="lifetime" role="img" aria-label={`${percent.toFixed(2)} percent of this read's observed lifetime was behind a blocked write`}>
		<div class="blocked-time" style:width={`${percent}%`}></div><div class="other-time" style:width={`${100 - percent}%`}></div>
	</div>
	<div class="timings">
		<div><span>Behind a blocked write</span><strong>{(trace.behind_write_ns / 1e9).toFixed(3)} s</strong><small>{percent.toFixed(2)}% of observed time</small></div>
		<div><span>After admission</span><strong>{ms(post)} ms</strong><small>Through guest notification</small></div>
	</div>
	<p class="caption">Another {ms(trace.admitted_ns - trace.behind_write_ns)} ms passed before admission. Total observed: {ms(trace.finished_ns)} ms. Timing starts when CAS sees the request in the ring; earlier guest waiting is excluded.</p>
	<details>
		<summary>Inside the {ms(post)} ms after admission</summary>
		<p class="caption">Queue {trace.queue}, request {trace.id}, disk offset {trace.offset}.</p>
		<dl>
			<div><dt>Frontend dispatch + command channel</dt><dd>{ms(phases.frontend_dispatch + phases.command_channel)} ms</dd></div>
			<div><dt>Reactor dispatch / write dependency</dt><dd>{ms(phases.reactor_dependency)} ms</dd></div>
			<div><dt>Prepare + advance the read</dt><dd>{ms(execution.prepare + execution.advance)} ms</dd></div>
			<div><dt>IO submission scheduler</dt><dd>{ms(execution.scheduler)} ms</dd></div>
			<div><dt>Manifest-page IO</dt><dd>{ms(execution.manifest)} ms</dd></div>
			<div><dt>Other reactor time</dt><dd>{ms(execution.other)} ms</dd></div>
			<div><dt>Response channel + guest completion</dt><dd>{ms(phases.response_channel + phases.guest_completion)} ms</dd></div>
		</dl>
		<p class="caption">All six selected tail reads hit the chunk cache: no WAL or chunk-payload IO, and no shared-fetch wait. IO timing includes kernel scheduling and completion reaping. These examples do not establish cold-read latency.</p>
	</details>

	<h3>Another CPU helps; it does not reserve a read queue</h3>
	<div class="table-scroll"><table>
		<thead><tr><th>Workload</th><th>Read max</th><th>Completion p99</th></tr></thead>
		<tbody>
			<tr><td>Read only</td><td>65–67 ms</td><td>2.54–2.61 ms</td></tr>
			<tr><td>Reader + writer, same CPU</td><td>2.71–7.93 s</td><td>325–2,869 ms</td></tr>
			<tr><td>Reader CPU 1, writer CPU 0</td><td>22 ms–2.21 s</td><td>4.62–6.46 ms</td></tr>
		</tbody>
	</table></div>
	<p class="explanation">Each guest mapped CPU 0 to queue 0 and CPU 1 to queue 1. A write still reached queue 1 in each separate-CPU pass, holding reads there for about 2.2 seconds. The trace confirms the write and queue; its guest thread or filesystem source remains unmeasured.</p>
	<p class="caption">Ranges cover both guests and two mixed passes. 4 KiB/QD8 reads, 1 MiB/QD32 writes, ten seconds per job; caches enabled, WALs drained between passes. Two-vCPU TCG guests on Spark. All 24 fio jobs and restart seed checks passed; no GC or OOM occurred. Test disks and keys were removed.</p>
	<details>
		<summary>Trace coverage, source paths and remaining measurements</summary>
		<p class="caption">This probe captured 385,700 of 385,860 reads. The follow-up fixes the cursor/peek observation race and passes 477 native tests; it was not rerun in a VM. Only the 32 slowest completed reads per image are retained. Status snapshots can lag stage boundaries, and tracing overhead is unmeasured.</p>
		<p class="caption">Next: define bounded read progress past a waiting write, preserving overlap, FLUSH and recovery rules. Cold reads, read-credit waits, telemetry freshness and long-history GC still need measurement.</p>
		<ul class="paths">
			<li><a href={source('crates/cas/daemon/src/backend.rs')}><code>crates/cas/daemon/src/backend.rs</code></a> · queue admission and guest completion</li>
			<li><a href={source('crates/cas/daemon/src/read_trace.rs')}><code>crates/cas/daemon/src/read_trace.rs</code></a> · measured observer</li>
			<li><a href={source('crates/cas/daemon/src/local/reactor.rs')}><code>crates/cas/daemon/src/local/reactor.rs</code></a> · dependency, scheduler and IO phases</li>
			<li><a href={record('docs/read-tracing.md')}>Trace field guide and corrected observer</a> · <a href={record('docs/validation/2026-09-14-read-pressure-trace.md')}>commands, failures and cleanup</a></li>
		</ul>
	</details>
</div>

<style>
	.read-pressure { border: 1px solid var(--border); border-radius: 0.5rem; padding: 1.25rem; margin: 1.5rem 0; }
	.finding { margin-top: 0; }
	.queue { display: flex; align-items: center; gap: 0.6rem; flex-wrap: wrap; padding: 1rem 0; font-size: 0.8rem; }
	.queue-label { font-size: 0.7rem; opacity: 0.7; margin-right: 0.4rem; }
	.blocked, .waiting { border: 1px solid var(--border); border-radius: 0.25rem; padding: 0.5rem 0.75rem; }
	.blocked { border-color: #b27c24; background: #b27c2418; }
	.blocked small { opacity: 0.7; }
	.explanation { font-size: 0.85rem; }
	label { display: block; margin-top: 1.5rem; font-size: 0.8rem; }
	select { display: block; margin: 0.5rem 0 1rem; width: 100%; max-width: 28rem; padding: 0.55rem; background: var(--background); color: inherit; border: 1px solid var(--border); border-radius: 0.25rem; font: inherit; font-size: 0.8rem; }
	select:focus-visible { outline: 2px solid currentColor; outline-offset: 3px; }
	.lifetime { display: flex; height: 0.8rem; border-radius: 0.15rem; overflow: hidden; }
	.blocked-time { background: #b27c24; }
	.other-time { background: #5081a3; }
	.timings { display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; margin-top: 0.8rem; }
	.timings > div { display: flex; flex-direction: column; gap: 0.2rem; }
	.timings span { font-size: 0.75rem; }
	.timings strong { font: 1.4rem ui-monospace, monospace; }
	.timings small { font-size: 0.65rem; opacity: 0.7; }
	.caption { font-size: 0.75rem; opacity: 0.75; }
	details { margin-top: 1rem; }
	summary { font-size: 0.8rem; cursor: pointer; }
	dl { font-size: 0.75rem; }
	dl > div { display: flex; justify-content: space-between; gap: 1rem; padding: 0.4rem 0; border-bottom: 1px solid var(--border); }
	dd { margin: 0; white-space: nowrap; font-variant-numeric: tabular-nums; }
	h3 { font-size: 1rem; margin-top: 1.7rem; }
	.table-scroll { overflow-x: auto; }
	table { border-collapse: collapse; width: 100%; font-size: 0.75rem; }
	th, td { text-align: left; padding: 0.65rem 0.4rem; border-bottom: 1px solid var(--border); }
	th { font-weight: 500; opacity: 0.7; }
	.paths { padding-left: 1rem; font-size: 0.75rem; }
	.paths li { margin: 0.4rem 0; }
	a, code { overflow-wrap: anywhere; }
	@media (max-width: 540px) { .read-pressure { padding: 1rem; } .queue { gap: 0.4rem; } .queue-label { width: 100%; } .timings strong { font-size: 1.15rem; } }
</style>
