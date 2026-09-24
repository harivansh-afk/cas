<script lang="ts">
	import { base } from '$app/paths';
	import '$lib/architecture/article.css';
	import Measurements from '$lib/architecture/Measurements.svelte';
	import Pressure from '$lib/architecture/Pressure.svelte';
	import ReadPressure from '$lib/architecture/ReadPressure.svelte';
	import ReadDecision from '$lib/architecture/ReadDecision.svelte';
	import ReadProgress from '$lib/architecture/ReadProgress.svelte';
	import Integration from '$lib/architecture/Integration.svelte';
	import work from '$lib/architecture/pressure-repeat.json';
	import data from '$lib/architecture/native.json';
	import type { NativeArm, NativeMetric, NativeReport, NativeValue } from '$lib/architecture/native';
	const report = data as NativeReport;
	const arms: { id: NativeArm; label: string }[] = [
		{ id: 'raw', label: 'Raw XFS' }, { id: 'daemon', label: 'Passthrough' }, { id: 'cas', label: 'CAS' }
	];
	const source = (path: string) => `https://git.harivan.sh/harivansh-afk/cas/src/commit/${report.source_revision}/${path}`;
	const number = (v: number) => v.toLocaleString('en-US', { maximumFractionDigits: 2 });
	const result = (value?: NativeValue) => value ? number(value.median) : 'No result';
	const range = (value?: NativeValue) => value ? `${number(value.min)} to ${number(value.max)}. ${value.n} runs.` : '';
	const mainRows = report.baseline.filter(row => ['read-q1-p99', 'write-q1-p99', 'flush-p99', 'sequential-read-mib-s'].includes(row.id));
</script>

<svelte:head>
	<title>Update 05 · CAS on CloudLab</title>
	<meta name="description" content="Spark latency fixes, native KVM tests, and the comparison of raw storage, passthrough, and CAS on CloudLab." />
</svelte:head>

{#snippet table(rows: NativeMetric[])}
	<div class="table-scroll"><table class="spec">
		<thead><tr><th>Measurement</th>{#each arms as arm}<th>{arm.label}</th>{/each}</tr></thead>
		<tbody>{#each rows as row}<tr>
			<td>{row.label}<small>{row.unit}</small></td>
			{#each arms as arm}<td><strong>{result(row.values[arm.id])}</strong><small>{range(row.values[arm.id])}</small></td>{/each}
		</tr>{/each}</tbody>
	</table></div>
{/snippet}

<article class="architecture" id="beginning">
	<header>
		<a class="back" href="{base}/">Index</a>
		<span class="eyebrow">Update 05 · 24 September 2026</span>
		<h1>CAS on CloudLab</h1>
		<ul>
			<li>The single-host backend is implemented.</li>
			<li>It has a write-ahead log, a compactor, shared chunks, and recovery tests.</li>
			<li>Native KVM tests now run on one CloudLab host.</li>
		</ul>
		<ul class="reference"><li><a href="{base}/updates/2/">Update 02</a> explains the design.</li><li><a href="{base}/updates/3/">Update 03</a> measures the scheduler fix.</li></ul>
	</header>

	<section id="spark">
		<h2>Spark tests</h2>
		<ul>
			<li>Writes filled their WAL quota and returned timeout errors.</li>
			<li>Capacity waits now stay pending. Reclamation wakes blocked requests.</li>
			<li>The compactor retains its scan position, grows buffers as needed, and writes only the final pages.</li>
		</ul>
		<div class="table-scroll"><table class="spec">
			<thead><tr><th>Work per MiB compacted</th><th>Before</th><th>After</th></tr></thead>
			<tbody>{#each work.comparison as row}<tr><td>{row.label}<small>{row.unit.replace(' / ', ' per ')}</small></td><td>{number(row.before)}</td><td>{number(row.after)}</td></tr>{/each}</tbody>
		</table></div>
		<ul>
			<li>The compaction changes reduced repeated work. Read stalls remained.</li>
			<li>Read bypass let independent reads pass blocked writes.</li>
			<li>A retry could still take the next turn before a waiting read. The scheduler now moves that retry behind other requests.</li>
			<li>The 15 September control had a mixed-read p99 of 219 to 287 ms. The fixed version measured 9 to 20 ms in the same session.</li>
			<li>Writers continued to progress. A read in one fixed run still took 25.5 seconds.</li>
		</ul>
		<ul class="reference">
			<li>Spark ran emulated guests inside a VM. Other work ran on the host.</li>
			<li>The compaction repeats had different backlogs. The table counts work per MiB. It does not measure a device speedup.</li>
		</ul>
		<details><summary>Spark measurements from 13 to 15 September</summary>
			<ul><li>These are the original dated panels.</li><li>Compare values within each experiment.</li></ul>
			<h3>13 September · raw, passthrough and CAS</h3><Measurements />
			<h3>14 September · capacity waits and compaction</h3><Pressure />
			<h3>14 September · read tracing</h3><ReadPressure /><ReadDecision />
			<h3>14 September · bounded read bypass</h3><ReadProgress />
			<h3>15 September · the same-session control</h3><Integration />
		</details>
	</section>

	<section id="native">
		<h2>CloudLab tests</h2>
		<ul>
			<li>The host has an EPYC 9354P, 32 cores, 192 GB RAM, and two 800 GB NVMe drives.</li>
			<li>CAS runs on the physical host. The guests use KVM.</li>
			<li>The controls use raw XFS and the passthrough daemon.</li>
			<li>All backends use the same guest, workload, and resource limits.</li>
			<li>{report.status}</li>
		</ul>
		{#if mainRows.length}
			{@render table(mainRows)}
			<ul class="reference"><li>Each cell shows the median, range, and number of runs.</li><li>fdatasync latency is separate from write completion.</li></ul>
			<details><summary>All native rows, pressure and accounting</summary>
				{@render table(report.baseline)}
				{#if report.pressure.length}<h3>One guest reads while another writes</h3>{@render table(report.pressure)}{/if}
				{#if report.accounting.length}<h3>One finite write, including drain</h3>{@render table(report.accounting)}{/if}
				<ul><li>{report.completed} runs completed.</li><li>The records retain {report.failed} failed attempts.</li>
				{#each report.failed_runs as run}<li><code>{run.name}</code>. {run.backend}. {run.error}</li>{/each}
				{#each report.notes as note}<li>{note}</li>{/each}</ul>
			</details>
		{/if}
		<details><summary>Test settings</summary>
			<ul>
				<li>Each guest has two vCPUs, 2 GiB RAM, and one queue of 128 entries.</li>
				<li>Each run has an 8 GiB cgroup limit and no swap.</li>
				<li>CPU affinity and memory policy use the NVMe's NUMA node.</li>
				<li>fio uses direct IO and a 512 MiB working set.</li>
				<li>CAS has a 16 MiB clean cache. The tests retain cache state between jobs.</li>
				<li>Each repeat starts with new storage. fio checks the data before and after.</li>
				<li>Read-only jobs start after compaction catches up.</li>
				<li>The pressure test puts a reader and a writer in separate guests. It does not test the remaining same-image FLUSH stalls.</li>
			</ul>
			<p><a href={source('docs/native-benchmark.md')}>Runner and method</a> · <a href={source('experiments/native/workload.sh')}>Workload</a> · <code>{report.source_revision.slice(0, 7)}</code></p>
		</details>
	</section>

	<section id="next">
		<h2>Open work</h2>
		<ul>
			<li>Run the full recovery suite on the measured revision.</li>
			<li>Complete the allocation audit and investigate the remaining read stalls.</li>
			<li>Build the ZFS comparison.</li>
			<li>Implement two-host storage, migration, and remote reads.</li>
			<li>Keep implementation and test runners in <code>cas</code>. Keep raw evidence and analysis in <code>cas-research</code>.</li>
		</ul>
	</section>
	<footer><a href="{base}/">Index</a><a href="#beginning">Top</a><a href="{base}/updates/3/">Update 03</a></footer>
</article>

<style>
	section > h2::before { content: none; }
	td small { display: block; margin-top: 0.25rem; font-size: 0.6875rem; color: var(--text-tertiary); font-weight: normal; }
	td strong { font-weight: 500; font-variant-numeric: tabular-nums; }
	details > h3 { margin-top: 2rem; font-size: 0.875rem; }
</style>
