<script lang="ts">
	/**
	 * The index table: number, title, one-line description, and a source link.
	 * The specification pages and the meeting updates share this one geometry.
	 */
	export interface Row {
		num: string;
		title: string;
		description: string;
		href: string;
		source: string;
	}

	/** Where the description sits: after the title, or flush against the right edge. */
	let { rows, label, align = 'start' }: { rows: Row[]; label: string; align?: 'start' | 'end' } = $props();
</script>

<nav class="toc" class:end={align === 'end'} aria-label={label}>
	{#each rows as row (row.num)}
		<div class="row">
			<a class="page" href={row.href}>
				<span class="toc-n">{row.num}</span>
				<span class="toc-t">{row.title}</span>
				<span class="toc-d">{row.description}</span>
			</a>
			<a class="src" href={row.source} target="_blank" rel="noopener" aria-label="source of {row.title} on GitHub" title="source on GitHub">
				<img src="https://github.com/harivansh-afk.png?size=64" alt="" width="16" height="16" loading="lazy" />
			</a>
		</div>
	{/each}
</nav>

<style>
	.toc {
		margin-top: 0.875rem;
		border-top: 1px solid var(--border);
	}
	.row {
		display: flex;
		align-items: stretch;
		border-bottom: 1px solid var(--border-subtle);
	}
	.row:hover {
		background: var(--background-secondary);
	}
	.page {
		flex: 1;
		min-width: 0;
		display: grid;
		grid-template-columns: 2.25rem max-content 1fr;
		gap: 0 1rem;
		padding: 0.5rem 0.375rem;
		color: var(--text-secondary);
		align-items: baseline;
	}
	.src {
		display: inline-flex;
		align-items: center;
		padding: 0 0.5rem;
		opacity: 0.6;
	}
	.src:hover {
		opacity: 1;
	}
	.src img {
		border-radius: 50%;
		display: block;
	}
	.toc-n {
		color: var(--text-quaternary);
		font-variant-numeric: tabular-nums;
	}
	.toc-t {
		color: var(--text-primary);
		font-weight: var(--weight-strong);
		white-space: nowrap;
	}
	.toc-d {
		color: var(--text-tertiary);
		font-size: 0.8125rem;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.end .toc-d {
		text-align: right;
		font-variant-numeric: tabular-nums;
	}
	@media (max-width: 640px) {
		.page {
			grid-template-columns: 2.25rem 1fr;
		}
		.toc-d {
			grid-column: 2;
			white-space: normal;
		}
		.end .toc-d {
			text-align: left;
		}
	}
</style>
