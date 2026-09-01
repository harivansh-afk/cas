<script lang="ts">
	import { pages } from '$lib/pages';

	let { num }: { num: string } = $props();

	const title = pages.find((p) => p.num === num)!.title;
	const last = pages[pages.length - 1].num;
</script>

<svelte:head>
	<title>local write, global chunks · {num} {title}</title>
</svelte:head>

<div class="pagehead">
	<span class="eyebrow">page {num} / {last}</span>
	<a class="byline" href="https://github.com/harivansh-afk" target="_blank" rel="noopener">
		<img src="https://github.com/harivansh-afk.png?size=64" alt="" width="16" height="16" loading="lazy" />
		harivansh-afk
	</a>
</div>
<h1>{title}</h1>

<style>
	.pagehead {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 0.75rem;
	}
	.byline {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		font-size: 0.75rem;
		color: var(--text-tertiary);
		white-space: nowrap;
	}
	.byline:hover {
		color: var(--text-primary);
	}
	.byline img {
		border-radius: 50%;
		display: block;
	}
</style>
