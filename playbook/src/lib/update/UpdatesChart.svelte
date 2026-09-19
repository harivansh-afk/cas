<script lang="ts">
	import { fleet } from '$lib/updates';

	const segments = [
		{ key: 'unique', label: 'unique' },
		{ key: 'base', label: 'duplicates already in the base' },
		{ key: 'novel', label: 'new duplicates, absent from the base' }
	] as const;
	const gb = (bytes: number) => `${(bytes / 1e9).toFixed(2)} GB`;
	const short = (label: string) => label.split(' · ')[0];
	const when = (label: string) => label.split(' · ')[1];
</script>

<figure aria-label="Content sharing across three cloned guests at T0, T1 and T2, for 4 KiB and 16 KiB chunks">
	<div class="groups">
		{#each fleet as group (group.label)}
			<div class="group">
				<h3>{group.label} chunks</h3>
				{#each group.periods as period (period.label)}
					{@const total = period.unique + period.base + period.novel}
					<div class="period">
						<span class="epoch"><strong>{short(period.label)}</strong> {when(period.label)}</span>
						<div class="bar" role="img" aria-label={`${period.label}: ${segments.map((s) => `${gb(period[s.key])} ${s.label}`).join(', ')}`}>
							{#each segments as segment (segment.key)}
								{@const share = period[segment.key] / total}
								<span class={segment.key} style:width="{100 * share}%" title="{segment.label}: {gb(period[segment.key])}">
									{#if segment.key !== 'novel' && share > 0.12}{gb(period[segment.key])}{/if}
								</span>
							{/each}
						</div>
						<span class="novel-value" class:zero={period.novel === 0}>{period.novel === 0 ? 'none new' : `${gb(period.novel)} new`}</span>
					</div>
				{/each}
			</div>
		{/each}
	</div>
	<figcaption>
		<div class="legend">{#each segments as segment (segment.key)}<span><i class={segment.key}></i>{segment.label}</span>{/each}</div>
		<p>Three guests, ancestor excluded. Bar length is what three separate stores would hold; only the unique part is stored when they share.</p>
	</figcaption>
</figure>

<style>
	figure { margin: 0; width: auto; }
	.groups { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 2.5rem; }
	h3 { margin: 0 0 0.75rem; font-size: 0.875rem; font-weight: var(--weight-strong); color: var(--text-primary); }
	.period { display: grid; grid-template-columns: 7.5rem 1fr 6rem; align-items: center; gap: 0.75rem; }
	.novel-value { font-size: 0.75rem; color: #d97706; font-weight: var(--weight-strong); font-variant-numeric: tabular-nums; white-space: nowrap; }
	.novel-value.zero { color: var(--text-quaternary); font-weight: var(--weight-body); }
	.period + .period { margin-top: 0.5rem; }
	.epoch { font-size: 0.75rem; color: var(--text-tertiary); }
	.epoch strong { color: var(--text-primary); margin-right: 0.35rem; }
	.bar { display: flex; width: 100%; height: 1.75rem; gap: 2px; }
	.bar span { display: flex; justify-content: center; align-items: center; font-size: 0.6875rem; white-space: nowrap; overflow: hidden; color: var(--text-primary); font-variant-numeric: tabular-nums; }
	.bar span:first-child { border-radius: 3px 0 0 3px; }
	.bar span:last-child { border-radius: 0 3px 3px 0; }
	.unique { background: var(--background-secondary); box-shadow: inset 0 0 0 1px var(--border); }
	.base { background: var(--text-quaternary); opacity: 0.55; }
	.novel { background: #d97706; }
	figcaption { margin: 1rem 0 0; padding: 0; border: 0; max-width: none; text-align: left; font-size: 0.75rem; line-height: 1.55; color: var(--text-tertiary); }
	.legend { display: flex; flex-wrap: wrap; gap: 0.3rem 1.25rem; color: var(--text-secondary); }
	.legend span { display: inline-flex; align-items: center; gap: 0.4rem; }
	i { width: 0.7rem; height: 0.7rem; display: inline-block; border-radius: 2px; }
	i.unique { box-shadow: inset 0 0 0 1px var(--text-quaternary); }
	p { margin: 0.5rem 0 0; }
	@media (max-width: 760px) { .groups { grid-template-columns: 1fr; gap: 1.5rem; } }
	@media (max-width: 520px) {
		.period { grid-template-columns: 1fr; gap: 0.2rem; }
		.novel-value { justify-self: end; }
		.bar span { font-size: 0; }
	}
	@media print {
		.bar span, i { print-color-adjust: exact; }
		.base { opacity: 0.45; }
	}
</style>
