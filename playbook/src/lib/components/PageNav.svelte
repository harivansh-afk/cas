<script lang="ts">
	import { base } from '$app/paths';
	import { pages } from '$lib/pages';

	let { num }: { num: string } = $props();

	const i = pages.findIndex((p) => p.num === num);
	const prev = i > 0 ? pages[i - 1] : null;
	const next = i < pages.length - 1 ? pages[i + 1] : null;
</script>

<nav class="pagenav">
	{#if prev}
		<a href="{base}/{prev.num}">← {prev.num} {prev.title}</a>
	{:else}
		<a href="{base}/">← index</a>
	{/if}
	<span class="spacer"></span>
	{#if next}
		<a href="{base}/{next.num}">{next.num} {next.title} →</a>
	{:else}
		<a href="{base}/">index →</a>
	{/if}
</nav>

<style>
	.pagenav {
		display: flex;
		justify-content: space-between;
		gap: 1rem;
		margin-top: 4rem;
		padding-top: 1rem;
		border-top: 1px solid var(--border);
		font-size: 0.75rem;
	}
	.pagenav a {
		color: var(--text-tertiary);
	}
	.pagenav a:hover {
		color: var(--text-primary);
	}
	.spacer {
		flex: 1;
	}
</style>
