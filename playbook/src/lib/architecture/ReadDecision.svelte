<script lang="ts">
	import data from './read-decision.json';
	const reportCommit = 'ddba79fe446d7794593fb37370984a144f17d2d9';
	const report = (file: string) => `https://git.harivan.sh/harivansh-afk/cas-research/src/commit/${reportCommit}/docs/measurements/read-decision-2026-09-14/${file}`;
	const source = (file: string) => `https://git.harivan.sh/harivansh-afk/cas-research/src/commit/${data.source_revision}/${file}`;
	const labels: Record<string, string> = { 'baseline-bpf': 'Read only', 'same-bpf': 'Reader and writer on CPU 0', 'separate-bpf': 'Reader on CPU 1, writer on CPU 0' };
	let selected = $state('same-bpf-1');
	const example = $derived(data.examples.find(e => e.id === selected)!);
	const ms = (ns: number) => (ns / 1e6).toFixed(3);
	const share = $derived(example.before_head_ns / example.fio_ns * 100);
	const groups = [
		{ label: 'Read only', stages: ['baseline'] },
		{ label: 'Reader + writer on CPU 0', stages: ['same-1', 'same-2'] },
		{ label: 'Reader CPU 1, writer CPU 0', stages: ['separate-1', 'separate-2'] }
	];
	function range(stages: string[], field: 'fio_read_p99_ns' | 'fio_read_max_ns') {
		const values = data.rows.filter(r => r.arm === 'off' && stages.includes(r.stage)).map(r => r[field] / 1e6);
		if (!values.length) return 'Pending';
		const min = Math.min(...values), max = Math.max(...values);
		if (min >= 1000) return `${(min / 1000).toFixed(2)}–${(max / 1000).toFixed(2)} s`;
		if (max >= 1000) return `${min.toFixed(1)} ms–${(max / 1000).toFixed(2)} s`;
		return `${min.toFixed(2)}–${max.toFixed(2)} ms`;
	}
</script>

<div class="decision">
	<p class="finding"><strong>The original long read wait was at admission.</strong> One matched read took 7.938 seconds in fio: 7.933 seconds passed before it reached the head of its CAS queue, then 3.53 ms after admission. The reader and the blocked write touched different disk ranges.</p>
	<div class="path" aria-label="Read path: guest submission, virtqueue, CAS admission, reactor and storage, then guest completion">
		<span><i>1</i>Guest<br /><small>fio → Linux bio</small></span>
		<span><i>2</i>Virtqueue<br /><small>shared descriptors</small></span>
		<span class="blocked"><i>3</i>CAS admission<br /><small>wait behind a write</small></span>
		<span><i>4</i>Reactor → storage<br /><small>cache, WAL or chunks</small></span>
		<span><i>5</i>Guest completion<br /><small>used ring → fio</small></span>
	</div>
	<p>In these measurements, the frontend released the queue mutex while waiting, but stopped consuming the queue at its blocked write. Later reads could not reach their own read credits. A lock-free queue would preserve that obstruction unless its admission rule also changed.</p>

	<label for="decision-read">Follow the same IO through fio, Linux and CAS</label>
	<select id="decision-read" bind:value={selected}>
		{#each data.examples as e}<option value={e.id}>{labels[e.stage]} · guest {e.guest}</option>{/each}
	</select>
	<div class="bar" role="img" aria-label={`${share.toFixed(2)} percent of this read's fio latency passed before it reached the CAS queue head`}>
		<div style:width={`${share}%`}></div>
	</div>
	<div class="numbers">
		<div><span>fio total</span><strong>{ms(example.fio_ns)} ms</strong></div>
		<div><span>Before CAS queue head</span><strong>{ms(example.before_head_ns)} ms</strong></div>
		<div><span>After CAS admission</span><strong>{ms(example.after_admission_ns)} ms</strong></div>
	</div>
	<p class="caption">Amber: before the CAS queue head. Blue: the rest of fio latency. Queue {example.queue}, disk offset {example.offset}. Selected slow IO, not a percentile sample; guest and CAS clocks are joined by request identity and only durations are compared.</p>
	<details>
		<summary>Where the remaining time went</summary>
		<dl>
			<div><dt>Guest bio submission → virtqueue publication</dt><dd>{ms(example.bio_to_publication_ns)} ms</dd></div>
			<div><dt>fio time outside bio submission → block completion</dt><dd>{ms(example.outside_bio_ns)} ms</dd></div>
			<div><dt>Virtqueue/completion time outside the CAS interval</dt><dd>{ms(example.outside_cas_ns)} ms</dd></div>
			<div><dt>At the CAS queue head</dt><dd>{ms(example.at_head_ns)} ms</dd></div>
			<div><dt>Frontend dispatch + command channel</dt><dd>{ms(example.phases.frontend_dispatch + example.phases.command_channel)} ms</dd></div>
			<div><dt>Reactor dispatch / publication dependency</dt><dd>{ms(example.phases.reactor)} ms</dd></div>
			<div><dt>Read execution</dt><dd>{ms(example.phases.execution)} ms</dd></div>
			<div><dt>Response + guest notification</dt><dd>{ms(example.phases.response_channel + example.phases.frontend_completion)} ms</dd></div>
		</dl>
		<p class="caption">The outside intervals combine work before and after an inner interval; they do not split one-way transport time. IO completion includes scheduling and reaping, not only device service. <a href={report('README.md')}>Full phase distributions, coverage and method</a>.</p>
	</details>

	<h3>Another CPU helps; the journal still uses its queue</h3>
	<p>Both separate-CPU probes found an 8 KiB journal write from <code>jbd2</code> blocking unrelated reads on queue 1 for 1.4–2.4 seconds. The reactor already separates reads from commands. The obstacle is earlier, where the frontend discovers requests.</p>
	<div class="table-scroll"><table>
		<thead><tr><th>Guest workload</th><th>Read p99</th><th>Slowest read</th></tr></thead>
		<tbody>{#each groups as group}<tr><td>{group.label}</td><td>{range(group.stages, 'fio_read_p99_ns')}</td><td>{range(group.stages, 'fio_read_max_ns')}</td></tr>{/each}</tbody>
	</table></div>
	<p class="caption">fio total latency with CAS tracing and guest probes off. Ranges cover both guests; mixed workloads have two passes. 4 KiB/QD8 reads and 1 MiB/QD32 writes, ten seconds per job. These are nested-VM development measurements, not native-drive latency or a latency guarantee.</p>
	<p>Cold read-only p99 was 3.69–4.18 ms; warm p99 was 1.97–2.02 ms. Warm mixed reads still reached p99 of 2.1–2.3 seconds with zero cache misses. Faster payload reads alone do not remove this queueing delay.</p>

	<h3>The design this led to</h3>
	<p><a href="https://git.harivan.sh/harivansh-afk/cas-research/pulls/53">PR #53</a> now retains bounded descriptor snapshots before storage admission. Independent reads can pass blocked writes; overlapping reads and FLUSH barriers keep their order. Waiting write payloads stay in guest RAM. The existing reactor and compactor still execute the work.</p>
	<p>These timing results predate the scheduler change. The live comparison above tests the merged implementation. <a href="https://git.harivan.sh/harivansh-afk/cas-research/src/commit/4b505548f97513d65fa25f193cca67dd83aa8ee5/docs/read-progress.md">Implementation, crate paths and limitations</a>.</p>
	<p>The cost is a fixed descriptor metadata quota and explicit dependency/recovery state. The new retained-carrier format also requires a fresh attachment when upgrading an existing guest. It also cannot help a read that the guest has not published. Cache sharding and thread pinning remain separate options for measured downstream costs. <a href={report('design.md')}>Alternatives, pros and cons, and required acceptance tests</a>.</p>
	<details>
		<summary>What was measured, and where to read the code</summary>
		<p>Instrumentation covers the guest bio/request path, queue admission reasons, publication dependencies, IO stages, cache lock waits and telemetry freshness. Histograms cover all traced completions; a bounded recent tail supplies examples. Guest probes measurably increase latency, so the table uses unprobed runs. The report also compares process-cold and warm caches.</p>
		<p>No scheduling policy changed in this experiment. Sustained native-host performance, full guest-ring exhaustion, remote reads, GC pressure and actual OOM/ENOSPC remain separate gates. The disposable labs were stopped and their disks and keys removed.</p>
		<ul>
			<li><a href={source('crates/cas/daemon/src/backend.rs')}><code>crates/cas/daemon/src/backend.rs</code></a> — virtqueue discovery and admission</li>
			<li><a href={source('crates/cas/daemon/src/read_trace.rs')}><code>crates/cas/daemon/src/read_trace.rs</code></a> — phases, gate reasons and retained examples</li>
			<li><a href={source('crates/cas/daemon/src/local/reactor/read.rs')}><code>crates/cas/daemon/src/local/reactor/read.rs</code></a> — read execution</li>
			<li><a href={source('crates/cas/core/src/cache.rs')}><code>crates/cas/core/src/cache.rs</code></a> — shared chunk-cache ownership</li>
			<li><a href={source('crates/harnesses/probes/read-path.bt')}><code>crates/harnesses/probes/read-path.bt</code></a> — guest request identities</li>
		</ul>
	</details>
</div>

<style>
	.decision { border: 1px solid var(--border); border-radius: .5rem; padding: 1.25rem; margin: 1.5rem 0; font-size: .85rem; }
	.finding { margin-top: 0; }
	.path { display: grid; grid-template-columns: repeat(5, minmax(0, 1fr)); gap: .5rem; margin: 1.25rem 0; font-size: .7rem; }
	.path span { padding: .6rem; border: 1px solid var(--border); border-radius: .3rem; }
	.path i { display: block; font-style: normal; opacity: .6; margin-bottom: .25rem; }
	.path small { opacity: .7; }
	.path .blocked { border-color: #b27c24; background: #b27c2418; }
	label { display: block; margin-top: 1.5rem; }
	select { margin: .5rem 0 1rem; width: 100%; padding: .6rem; background: var(--background); color: inherit; border: 1px solid var(--border); border-radius: .25rem; font: inherit; }
	select:focus-visible { outline: 2px solid currentColor; outline-offset: 3px; }
	.bar { height: .75rem; background: #5081a3; border-radius: .15rem; overflow: hidden; }
	.bar div { height: 100%; background: #b27c24; }
	.numbers { display: grid; grid-template-columns: repeat(3, 1fr); gap: .75rem; margin: .75rem 0; }
	.numbers > div { display: flex; flex-direction: column; gap: .3rem; }
	.numbers span { font-size: .7rem; }
	.numbers strong { font: 1rem ui-monospace, monospace; }
	.caption { font-size: .75rem; opacity: .75; }
	details { margin-top: 1rem; }
	summary { cursor: pointer; }
	h3 { margin-top: 1.75rem; font-size: 1rem; }
	dl > div { display: flex; justify-content: space-between; gap: 1rem; border-bottom: 1px solid var(--border); padding: .45rem 0; }
	dl { font-size: .75rem; }
	dd { margin: 0; white-space: nowrap; }
	.table-scroll { overflow-x: auto; }
	table { width: 100%; border-collapse: collapse; font-size: .75rem; }
	th, td { text-align: left; padding: .65rem .35rem; border-bottom: 1px solid var(--border); }
	th { opacity: .7; font-weight: 500; }
	li { margin: .5rem 0; }
	a, code { overflow-wrap: anywhere; }
	@media (max-width: 540px) { .decision { padding: 1rem; } .path { grid-template-columns: 1fr; gap: .35rem; } .path span { position: relative; padding: .5rem .5rem .5rem 2rem; } .path i { position: absolute; left: .65rem; top: .5rem; } .path br { display: none; } .path small { display: block; } .numbers { grid-template-columns: 1fr; } .numbers > div { flex-direction: row; align-items: baseline; justify-content: space-between; } }
</style>
