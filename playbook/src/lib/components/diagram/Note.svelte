<svelte:options namespace="svg" />

<script lang="ts">
	/** Free text in the margin of a diagram. */
	let {
		x,
		y,
		text,
		anchor = 'start',
		tone = 'default',
		size = 10.5
	}: {
		x: number;
		y: number;
		text: string | string[];
		anchor?: 'start' | 'middle' | 'end';
		tone?: 'default' | 'accent' | 'muted';
		size?: number;
	} = $props();

	const lines = $derived(Array.isArray(text) ? text : [text]);
</script>

<g>
	{#each lines as line, i (i)}
		<text {x} y={y + i * (size + 4)} text-anchor={anchor} font-size={size} fill={tone === 'accent' ? '#d97706' : 'currentColor'} opacity={tone === 'muted' ? 0.55 : tone === 'accent' ? 1 : 0.8}>{line}</text>
	{/each}
</g>
