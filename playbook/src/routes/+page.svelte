<script lang="ts">
	import { base } from '$app/paths';
	import { pages, source } from '$lib/pages';
</script>

<svelte:head>
	<title>Content, not lineage</title>
	<meta
		name="description"
		content="Every published dedup ratio counts bytes that clones would have shared for free. This study subtracts them, splits the rest between what an aligned dedup table reaches and what only content-defined chunking reaches, and prices both on stock systems."
	/>
</svelte:head>

<span class="eyebrow">CS 4993 · fall 2026 · research spec</span>
<p class="lede">
	<mark>Every published dedup ratio counts bytes that clones would have shared for free.</mark>
	This study subtracts them, splits the rest between what an aligned dedup table reaches and what
	only content-defined chunking reaches, and prices both on systems an operator can turn on today.
</p>

<nav class="toc" aria-label="Pages">
	{#each pages as { num, title, description, draft } (num)}
		<div class="row" class:draft>
			<a class="page" href="{base}/{num}">
				<span class="toc-n">{num}</span>
				<span class="toc-t">{title}{#if draft}<span class="tag">draft</span>{/if}</span>
				<span class="toc-d">{description}</span>
			</a>
			<a class="src" href={source(num)} target="_blank" rel="noopener" aria-label="source of page {num} on GitHub" title="source on GitHub">
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
		padding: 0 0.5rem 0 0.75rem;
		color: var(--text-quaternary);
		transition: color 0.12s ease;
	}
	.src img {
		display: block;
		border-radius: 50%;
		opacity: 0.8;
		transition: opacity 0.12s ease;
	}
	.src:hover img,
	.src:focus-visible img {
		opacity: 1;
	}
	.draft .src img {
		opacity: 0.35;
		filter: grayscale(1);
	}
	.toc-n {
		font-size: 0.75rem;
		font-weight: var(--weight-medium);
		color: var(--text-quaternary);
		font-variant-numeric: tabular-nums;
	}
	.toc-t {
		font-size: 0.875rem;
		font-weight: var(--weight-strong);
		color: var(--text-primary);
		white-space: nowrap;
	}
	.toc-d {
		font-size: 0.75rem;
		color: var(--text-tertiary);
		text-align: right;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	/* draft rows: same shape, one shade down everywhere, no bold */
	.draft .toc-n,
	.draft .toc-t,
	.draft .toc-d {
		color: var(--text-quaternary);
	}
	.draft .toc-t {
		font-weight: var(--weight-medium);
	}
	.tag {
		margin-left: 0.5rem;
		font-size: 0.625rem;
		font-weight: var(--weight-medium);
		text-transform: uppercase;
		letter-spacing: 0.1em;
		color: var(--text-quaternary);
		border: 1px solid var(--border);
		border-radius: 3px;
		padding: 0.05rem 0.3rem;
		vertical-align: 0.1em;
	}

	@media (max-width: 640px) {
		.page {
			grid-template-columns: 2.25rem 1fr;
		}
		.toc-d {
			display: none;
		}
	}
</style>
