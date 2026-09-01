<script lang="ts">
	import { base } from '$app/paths';
	import { pages } from '$lib/pages';
</script>

<svelte:head>
	<title>Chunk pointers, not block pointers</title>
	<meta
		name="description"
		content="Measure whether chunk-level content addressing finds enough duplicate data that copy-on-write systems structurally miss to justify its runtime costs."
	/>
</svelte:head>

<span class="eyebrow">CS 4993 · fall 2026</span>
<h1>Chunk pointers, not block pointers</h1>

<nav class="toc" aria-label="Pages">
	{#each pages as { num, title, description } (num)}
		<a href="{base}/{num}">
			<span class="toc-n">{num}</span>
			<span class="toc-t">{title}</span>
			<span class="toc-d">{description}</span>
		</a>
	{/each}
</nav>

<p class="homefoot">spec v4 · thesis · model · distribution · measurement · implementation</p>

<style>
	.toc {
		margin-top: 1.5rem;
		border-top: 1px solid var(--border);
	}
	.toc a {
		display: grid;
		grid-template-columns: 2.5rem 1fr;
		gap: 0 1rem;
		padding: 0.55rem 0.5rem;
		border-bottom: 1px solid var(--border);
		color: var(--text-secondary);
		align-items: baseline;
	}
	.toc a:hover {
		background: var(--background-secondary);
	}
	.toc-n {
		font-size: 0.8125rem;
		font-weight: var(--weight-strong);
		color: var(--text-quaternary);
		font-variant-numeric: tabular-nums;
	}
	.toc-t {
		font-size: 0.9375rem;
		font-weight: var(--weight-strong);
		color: var(--text-primary);
	}
	.toc-d {
		grid-column: 2;
		font-size: 0.8125rem;
		color: var(--text-tertiary);
		margin-top: 0.125rem;
	}
	.homefoot {
		margin-top: 1.5rem;
		font-size: 0.6875rem;
		color: var(--text-quaternary);
	}
</style>
