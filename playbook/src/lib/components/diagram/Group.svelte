<svelte:options namespace="svg" />

<script lang="ts">
	/** A dashed boundary with a small uppercase label in its top-left corner. */
	let {
		x,
		y,
		w,
		h,
		label,
		tone = 'default'
	}: { x: number; y: number; w: number; h: number; label: string; tone?: 'default' | 'accent' } = $props();

	const stroke = $derived(tone === 'accent' ? '#d97706' : 'currentColor');
</script>

<g>
	<rect {x} {y} width={w} height={h} rx="6" fill="none" {stroke} stroke-width="1" stroke-dasharray="5 4" opacity={tone === 'accent' ? 0.8 : 0.4} />
	<text x={x + 12} y={y + 16} font-size="9.5" font-weight="500" letter-spacing="0.1em" fill={stroke} opacity={tone === 'accent' ? 1 : 0.7}>{label.toUpperCase()}</text>
</g>
