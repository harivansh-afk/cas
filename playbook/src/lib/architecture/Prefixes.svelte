<script lang="ts">
	let selected = $state(0);
	const states = [
		{ title: 'Before A', p: 0, e: 0, d: 0, text: 'Start from an empty image. The counters below are mutation sequences, not byte offsets. This is an illustrative execution, not a recorded timing trace.' },
		{ title: 'WRITE A completes', p: 1, e: 0, d: 0, text: 'Mutation 1 is fully appended and published. P is recorded before the guest used entry. Reads can see A. No successful sync has established E=1 yet; a host power loss may still lose A.' },
		{ title: 'FLUSH completes', p: 1, e: 1, d: 0, text: 'The FENCE covering A and its earlier writes have completed, then fdatasync has succeeded. A is durable in the WAL. The compactor has not run, so there may still be no chunk for A.' },
		{ title: 'WRITE B completes', p: 2, e: 1, d: 0, text: 'B overwrites the same logical block at sequence 2. A remains the durable prefix, while reads see B from staging. The unsynced B must not be mistaken for a durable manifest version.' },
		{ title: 'Compact through A', p: 2, e: 1, d: 1, text: 'The compactor may publish a manifest containing A through sequence 1. The newer B mapping remains in staging and wins every subsequent lookup. Publishing D never erases a later overwrite.' },
		{ title: 'Sync and compact B', p: 2, e: 2, d: 2, text: 'After another successful fence/sync, E reaches 2. After chunk and manifest synchronization, D reaches 2. B can now be read through the manifest. Old staging payload is reclaimable only after its pins retire.' }
	];
	const snapshot = $derived(states[selected]);
</script>

<div class="prefixes">
	<p class="label">Follow one block through overwrite and compaction</p>
	<div class="choices" role="group" aria-label="Illustrative storage states">
		{#each states as item, i}
			<button type="button" aria-pressed={selected === i} onclick={() => selected = i}>{i + 1}. {item.title}</button>
		{/each}
	</div>
	<div aria-live="polite" aria-atomic="true">
		<div class="counters">
			{#each [{ label: 'P · published', value: snapshot.p }, { label: 'E · synced WAL', value: snapshot.e }, { label: 'D · manifest', value: snapshot.d }] as counter}
				<div><span>{counter.label}</span><strong>{counter.value}</strong><div class="track"><div style:width={`${counter.value * 50}%`}></div></div></div>
			{/each}
		</div>
		<p class="explanation">{snapshot.text}</p>
	</div>
</div>

<style>
	.prefixes { padding: 1.25rem; background: var(--background-secondary); border: 1px solid var(--border); border-radius: var(--radius); margin: 1.5rem 0; }
	.label { color: var(--text-primary); font-weight: var(--weight-strong); }
	.choices { display: flex; flex-wrap: wrap; gap: 0.5rem; margin-bottom: 1.5rem; }
	button { font: inherit; font-size: 0.75rem; line-height: 1.5; padding: 0.4rem 0.6rem; border: 1px solid var(--border); border-radius: 4px; color: var(--text-secondary); background: var(--surface); cursor: pointer; text-align: left; }
	button[aria-pressed='true'] { border-color: var(--text-primary); background: var(--text-primary); color: var(--background); }
	button:focus-visible { outline: 2px solid var(--text-primary); outline-offset: 3px; }
	.counters { display: grid; grid-template-columns: repeat(3, 1fr); gap: 1.25rem; }
	.counters span { display: block; font-size: 0.6875rem; color: var(--text-tertiary); }
	.counters strong { display: block; font-size: 1.5rem; }
	.track { background: var(--border); height: 4px; margin-top: 0.25rem; }
	.track div { height: 100%; background: #d97706; }
	.explanation { min-height: 7em; margin: 1.25rem 0 0; font-size: 0.8125rem; }
	@media print { .choices { display: none; } }
</style>
