<script lang="ts">
	import { base } from '$app/paths';
	import { pages } from '$lib/pages';
</script>

<svelte:head>
	<title>Chunk pointers, not block pointers</title>
	<meta
		name="description"
		content="Every published dedup ratio counts bytes that copy-on-write clones would have shared for free; this study subtracts them and measures what content addressing alone is worth."
	/>
</svelte:head>

<span class="eyebrow">CS 4993 · fall 2026 · research spec</span>
<p class="lede">
	<mark>Every published dedup ratio counts bytes that copy-on-write clones would have shared for
	free</mark>; this study subtracts them and measures what content addressing alone is worth.
</p>

<nav class="toc" aria-label="Pages">
	{#each pages as { num, title, description } (num)}
		<a href="{base}/{num}">
			<span class="toc-n">{num}</span>
			<span class="toc-t">{title}</span>
			<span class="toc-d">{description}</span>
		</a>
	{/each}
</nav>

<style>
	.toc {
		margin-top: 0.875rem;
		border-top: 1px solid var(--border);
	}
	.toc a {
		display: grid;
		grid-template-columns: 2.25rem max-content 1fr;
		gap: 0 1rem;
		padding: 0.5rem 0.375rem;
		border-bottom: 1px solid var(--border-subtle);
		color: var(--text-secondary);
		align-items: baseline;
	}
	.toc a:hover {
		background: var(--background-secondary);
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
	@media (max-width: 640px) {
		.toc a {
			grid-template-columns: 2.25rem 1fr;
		}
		.toc-d {
			display: none;
		}
	}
</style>
