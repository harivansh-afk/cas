<script lang="ts">
	import { pages } from '$lib/pages';

	let { num }: { num: string } = $props();

	const title = pages.find((p) => p.num === num)!.title;
</script>

<svelte:head>
	<title>casblk · {num} {title}</title>
</svelte:head>

<div class="pagehead">
	<span class="eyebrow">casblk · page {num}</span>
	<a class="backlink" href="/">← index</a>
</div>
<h1>{title}</h1>

<style>
	.pagehead {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
		gap: 0.75rem;
	}
	.backlink {
		font-size: 0.75rem;
		color: var(--text-tertiary);
		white-space: nowrap;
	}
	.backlink:hover {
		color: var(--text-primary);
	}
</style>
