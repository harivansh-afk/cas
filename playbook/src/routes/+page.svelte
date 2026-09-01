<script lang="ts">
	import { base } from '$app/paths';
	import { pages, source } from '$lib/pages';
</script>

<svelte:head>
	<title>content addressing across hosts</title>
	<meta
		name="description"
		content="A dedup table shares duplicate data within one host. Content addressing shares it across hosts. This study builds a content-addressed block backend under stock QEMU and measures what crossing the host buys and what the cold read costs."
	/>
</svelte:head>

<span class="eyebrow">CS 4993 · fall 2026 · research spec</span>
<p class="lede">
	<mark>A dedup table shares duplicate data within one host. Content addressing shares it across hosts.</mark>
	This study builds a content-addressed block backend under stock QEMU, with the network on the read path only, and measures what crossing the host buys and what a cold read costs.
</p>

<nav class="toc" aria-label="Pages">
	{#each pages as { num, title, description } (num)}
		<div class="row">
			<a class="page" href="{base}/{num}">
				<span class="toc-n">{num}</span>
				<span class="toc-t">{title}</span>
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
	@media (max-width: 640px) {
		.page {
			grid-template-columns: 2.25rem 1fr;
		}
		.toc-d {
			grid-column: 2;
			white-space: normal;
		}
	}
</style>
