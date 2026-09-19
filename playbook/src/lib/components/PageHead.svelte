<script lang="ts">
	import { pages } from '$lib/pages';

	let { num }: { num: string } = $props();

	const title = pages.find((p) => p.num === num)!.title;
	const last = pages[pages.length - 1].num;
</script>

<svelte:head>
	<title>Content-addressed deduplication · {num} {title}</title>
</svelte:head>

<span class="eyebrow">page {num} / {last}</span>
<h1>{title}</h1>
