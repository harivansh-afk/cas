<script lang="ts">
	import status from './integration.json';
	const lab = status.lab;
	type ArmId = keyof typeof lab.final;
	const arms = lab.arms as { id: ArmId; source: string; label: string }[];
	function ms(values: number[]) {
		const [lo, hi] = [Math.min(...values), Math.max(...values)];
		const one = (v: number) => (v >= 1000 ? `${(v / 1000).toFixed(1)} s` : `${v.toFixed(v < 10 ? 1 : 0)} ms`);
		return lo === hi ? one(lo) : `${one(lo)}–${one(hi)}`;
	}
	function count(values: number[]) {
		const k = (v: number) => `${(v / 1000).toFixed(v < 10000 ? 1 : 0)}k`;
		return `${k(Math.min(...values))}–${k(Math.max(...values))}`;
	}
	function rate(values: number[]) {
		if (!values.some((v) => v > 0)) return '—';
		const [lo, hi] = [Math.min(...values), Math.max(...values)].map((v) => v.toFixed(v < 10 ? 1 : 0));
		return lo === hi ? lo : `${lo}–${hi}`;
	}
	const seconds = (v: number) => `${(v / 1000).toFixed(v >= 1000 ? 1 : 3)} s`;
	let selected = $state<ArmId>('control');
	const final = $derived(lab.final[selected]);
</script>

<div class="integration">
	<p class="finding"><strong>Independent reads waited up to {seconds(Math.max(...lab.final.control.read_admission_max_wait_ms))} in admission before the fix and at most {seconds(Math.max(...lab.final.off.read_admission_max_wait_ms, ...lab.final.off2.read_admission_max_wait_ms))} after it.</strong> Three back-to-back runs in one host session, with the same workload.</p>
	<!-- svelte-ignore a11y_no_noninteractive_tabindex (The comparison scrolls horizontally on small screens.) -->
	<div class="table-scroll" role="region" aria-label="Read latency by arm" tabindex="0"><table class="spec">
		<thead><tr><th>Workload</th><th>Arm</th><th>Read p99</th><th>Slowest read</th><th>Reads per 10 s</th><th>Write MiB/s</th></tr></thead>
		<tbody>{#each lab.workloads as workload}{#each arms as arm, i}<tr>
			{#if i === 0}<td rowspan={arms.length}>{workload.label}</td>{/if}
			<td>{arm.label}</td>
			<td>{ms(workload.arms[arm.id].p99_ms)}</td>
			<td>{ms(workload.arms[arm.id].max_ms)}</td>
			<td>{count(workload.arms[arm.id].read_ios)}</td>
			<td>{rate(workload.arms[arm.id].write_mib_s)}</td>
		</tr>{/each}{/each}</tbody>
	</table></div>
	<p class="caption">fio total latency, 4 KiB/QD8 reads and 1 MiB/QD32 writes, ten-second windows, two guests per arm; ranges cover both guests and both passes of each mixed workload. Every job and every final 64 MiB CRC check passed; no storage errors. Nested TCG guests on a busy shared host: {lab.host_note}</p>

	<h3>Scheduler counters at the end of each arm</h3>
	<div class="choices" aria-label="Arm">{#each arms as arm}<button type="button" aria-pressed={selected === arm.id} onclick={() => (selected = arm.id)}>{arm.label}</button>{/each}</div>
	<dl aria-live="polite">
		<div><dt>Independent-read admission, longest wait</dt><dd>{final.read_admission_max_wait_ms.map(seconds).join(' / ')}</dd></div>
		<div><dt>Ordinary queue head, longest wait</dt><dd>{final.head_max_wait_ms.map(seconds).join(' / ')}</dd></div>
		<div><dt>Reads that passed a blocked head</dt><dd>{final.bypassed.map((v: number) => v.toLocaleString('en-US')).join(' / ')}</dd></div>
		<div><dt>Refused scheduler turns</dt><dd>{final.refused_turns.map((v: number) => v.toLocaleString('en-US')).join(' / ')}</dd></div>
		<div><dt>Admitted requests</dt><dd>{final.admitted.map((v: number) => v.toLocaleString('en-US')).join(' / ')}</dd></div>
	</dl>
	<p class="caption">Per image (guest 1 / guest 2), from the daemon’s own telemetry at the end of the arm. Reads at the head of their own queue count with the ordinary heads.</p>
</div>

<style>
	.integration { border: 1px solid var(--border); border-radius: 0.5rem; padding: 1.25rem; margin: 1.5rem 0; font-size: 0.85rem; }
	.finding { margin-top: 0; }
	.caption { font-size: 0.75rem; opacity: 0.75; }
	h3 { margin-top: 1.5rem; font-size: 1rem; }
	.choices { display: flex; flex-wrap: wrap; gap: 0.4rem; margin-bottom: 0.8rem; }
	button { border: 1px solid var(--border); border-radius: 0.3rem; background: transparent; color: inherit; padding: 0.5rem 0.65rem; font: inherit; font-size: 0.75rem; cursor: pointer; }
	button[aria-pressed='true'] { background: #5081a320; border-color: #5081a3; }
	button:focus-visible { outline: 2px solid currentColor; outline-offset: 3px; }
	dl { display: grid; grid-template-columns: 1fr auto; gap: 0.5rem 1.5rem; margin: 0; font-size: 0.8rem; }
	dl > div { display: contents; }
	dt { opacity: 0.8; }
	dd { margin: 0; text-align: right; white-space: nowrap; font-variant-numeric: tabular-nums; }
	td:not(:first-child) { white-space: nowrap; }
	@media (max-width: 540px) { .integration { padding: 1rem; } dl { grid-template-columns: 1fr; } dd { text-align: left; } }
</style>
