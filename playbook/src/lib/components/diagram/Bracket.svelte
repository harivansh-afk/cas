<svelte:options namespace="svg" />

<script lang="ts">
	/** A vertical bracket to the right of a span of nodes, with a label. */
	let {
		x,
		y1,
		y2,
		label
	}: { x: number; y1: number; y2: number; label: string | string[] } = $props();

	const lines = $derived(Array.isArray(label) ? label : [label]);
	const mid = $derived((y1 + y2) / 2);
</script>

<g>
	<path d="M{x} {y1} H{x + 8} V{y2} H{x}" fill="none" stroke="#d97706" stroke-width="1.25" />
	{#each lines as line, i (i)}
		<text x={x + 16} y={mid - ((lines.length - 1) * 14) / 2 + i * 14 + 4} font-size="10.5" fill="#d97706" font-weight={i === 0 ? 600 : 400} opacity={i === 0 ? 1 : 0.8}>{line}</text>
	{/each}
</g>
