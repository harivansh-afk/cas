<script lang="ts">
	import data from './read-progress.json';
	const reportCommit = 'e741b4df78710ecbdead555af8d8d6dde2d5f834';
	const report = `https://git.harivan.sh/harivansh-afk/cas-research/src/commit/${reportCommit}/docs/measurements/read-progress-live-2026-09-14/README.md`;
	const source = (path: string) => `https://git.harivan.sh/harivansh-afk/cas-research/src/commit/${data.source_revision}/${path}`;
	function duration(ns: number) {
		return ns >= 1e9 ? `${(ns / 1e9).toFixed(2)} s` : `${(ns / 1e6).toFixed(2)} ms`;
	}
	function range(values: number[]) {
		return `${duration(Math.min(...values))}–${duration(Math.max(...values))}`;
	}
	const cases = [
		{ label: 'Independent read', range: 'B', barrier: false, passes: true, explanation: 'Read B can use its own read credits while Write A waits for WAL capacity. The write payload remains in guest RAM.' },
		{ label: 'Overlapping read', range: 'A', barrier: false, passes: false, explanation: 'Read A waits for the earlier write to publish. Passing it would return the wrong version of those bytes.' },
		{ label: 'FLUSH in between', range: 'B', barrier: true, passes: false, explanation: 'FLUSH is a barrier. Even an unrelated read waits until the earlier requests have reached admission; the existing durability rules then apply.' }
	];
	let selected = $state(0);
	const example = $derived(cases[selected]);
</script>

<div class="progress">
	<p class="finding"><strong>{data.finding}</strong> The longest untraced read still took {duration(Math.max(...data.comparison.flatMap(row => row.after_max_ns)))}.</p>
	<!-- svelte-ignore a11y_no_noninteractive_tabindex (The comparison scrolls horizontally on small screens.) -->
	<div class="table-scroll" role="region" aria-label="Read latency comparison" tabindex="0"><table>
		<thead><tr><th>Workload</th><th>Earlier read p99</th><th>Merged read p99</th><th>Earlier slowest</th><th>Merged slowest</th></tr></thead>
		<tbody>{#each data.comparison as row}<tr><td>{row.label}</td><td>{range(row.before_p99_ns)}</td><td>{range(row.after_p99_ns)}</td><td>{duration(Math.max(...row.before_max_ns))}</td><td>{duration(Math.max(...row.after_max_ns))}</td></tr>{/each}</tbody>
	</table></div>
	<p class="caption">Untraced fio total latency. Each p99 range covers two guests; mixed cases have two passes. 4 KiB/QD8 reads and 1 MiB/QD32 writes, ten-second windows. Matched workloads ran in separate Spark sessions using nested TCG guests; these are development measurements.</p>

	<h3>The admission rule</h3>
	<div class="choices" aria-label="Read ordering examples">
		{#each cases as item, i}<button type="button" aria-pressed={selected === i} onclick={() => selected = i}>{item.label}</button>{/each}
	</div>
	<div class="queue" aria-label="Illustrative queue order; A and B are non-overlapping disk ranges">
		<div class="waiting"><small>Published first</small><strong>Write A</strong><span>Waiting for WAL space</span></div>
		{#if example.barrier}<div class="waiting"><small>Barrier</small><strong>FLUSH</strong><span>Waits for earlier work</span></div>{/if}
		<div class:passing={example.passes} class:waiting={!example.passes}><small>Published later</small><strong>Read {example.range}</strong><span>{example.passes ? 'Eligible for read admission' : 'Waits for its dependency'}</span></div>
	</div>
	<p aria-live="polite">{example.explanation}</p>
	<p class="caption">A and B stand for different disk ranges. CAS owns bounded descriptor snapshots before admission; it does not need a new guest queue, a lock-free queue or a new executor. After admission, the existing image-wide write-publication dependency still applies.</p>

	<h3>What the instrumentation says now</h3>
	<p>{data.attribution}</p>
	<p>Each image retries its oldest write before its eligible read. That retry marks the write ready again, which can displace the read the shared scheduler just selected. Two images can keep waking each other and repeating this until write capacity returns.</p>
	<p>{data.remaining}</p>
	<details>
		<summary>Validation, resource costs and code</summary>
		<p>The packaged merge passed 499 native tests. All {data.recovery_cases} live recovery/reset scenarios and {data.crc_checks} full 64 MiB seed checks passed. The recovery cases kept QEMU alive across backend replacement; they did not reproduce an overloaded deferred-read crash. Native frontend tests cover that ownership case.</p>
		<p>The largest sampled lab memory peak was {data.peak_gib.toFixed(2)} GiB under a 6 GiB cap, with zero observed cgroup OOM or limit events. Disposable disks and keys were removed. Actual OOM/ENOSPC, sustained fairness, full guest-ring exhaustion, long-history GC and physical power loss remain separate gates.</p>
		<p>Each frontend reserves about 6 MiB of descriptor metadata quota, with eight read-request slots separate from writes. The existing byte allowance fits seven simultaneous 4 KiB reads. Version-3 retained carriers require a fresh guest attachment when upgrading from version 2.</p>
		<ul>
			<li><a href={source('crates/cas/daemon/src/backend/frontier.rs')}><code>backend/frontier.rs</code></a> — discover, check dependencies, admit independent reads</li>
			<li><a href={source('crates/cas/daemon/src/local/pools.rs')}><code>local/pools.rs</code></a> — separate read credits</li>
			<li><a href={source('crates/cas/daemon/src/inflight.rs')}><code>inflight.rs</code></a> and <a href={source('crates/cas/daemon/src/backend/recovery.rs')}><code>backend/recovery.rs</code></a> — retain and recover ownership</li>
			<li><a href={source('crates/cas/daemon/src/read_trace.rs')}><code>read_trace.rs</code></a> — phase timings and admission observations</li>
		</ul>
		<p class="caption">All four owners are in <code>crates/cas/daemon</code>. <a href={report}>Raw-result analysis, per-job numbers and limitations</a>.</p>
	</details>
</div>

<style>
	.progress { border: 1px solid var(--border); border-radius: .5rem; padding: 1.25rem; margin: 1.5rem 0; font-size: .85rem; }
	.finding { margin-top: 0; }
	.table-scroll { overflow-x: auto; }
	.table-scroll:focus-visible { outline: 2px solid currentColor; outline-offset: 3px; }
	table { width: 100%; min-width: 600px; border-collapse: collapse; font-size: .75rem; }
	th, td { text-align: left; padding: .65rem .4rem; border-bottom: 1px solid var(--border); }
	th:first-child, td:first-child { min-width: 135px; }
	th { opacity: .7; font-weight: 500; }
	td:not(:first-child) { white-space: nowrap; }
	h3 { margin-top: 1.5rem; font-size: 1rem; }
	.caption { font-size: .75rem; opacity: .75; }
	.choices { display: flex; flex-wrap: wrap; gap: .4rem; margin-bottom: .8rem; }
	button { border: 1px solid var(--border); border-radius: .3rem; background: transparent; color: inherit; padding: .5rem .65rem; font: inherit; font-size: .75rem; cursor: pointer; }
	button[aria-pressed="true"] { background: #5081a320; border-color: #5081a3; }
	button:focus-visible { outline: 2px solid currentColor; outline-offset: 3px; }
	.queue { display: flex; flex-wrap: wrap; gap: .6rem; }
	.queue > div { display: flex; flex: 1 1 130px; flex-direction: column; gap: .4rem; padding: .8rem; border: 1px solid; border-radius: .3rem; }
	.queue small, .queue span { font-size: .7rem; }
	.queue > .waiting { border-color: #b27c24; background: #b27c2418; }
	.queue > .passing { border-color: #5081a3; background: #5081a318; }
	details { margin-top: 1rem; }
	summary { cursor: pointer; }
	li { margin: .5rem 0; }
	a, code { overflow-wrap: anywhere; }
	@media (max-width: 540px) { .progress { padding: 1rem; } }
</style>
