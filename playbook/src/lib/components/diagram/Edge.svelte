<svelte:options namespace="svg" />

<script lang="ts">
	import { getContext } from 'svelte';

	/**
	 * A connector. Give it `points` (at least two); it draws a polyline with an arrowhead.
	 * The label sits beside the midpoint of the last segment.
	 */
	let {
		points,
		label,
		labelDy = -6,
		labelDx = 0,
		tone = 'default',
		dashed = false,
		arrow = true
	}: {
		points: [number, number][];
		label?: string;
		labelDy?: number;
		labelDx?: number;
		tone?: 'default' | 'accent' | 'muted';
		dashed?: boolean;
		arrow?: boolean;
	} = $props();

	const { uid } = getContext<{ uid: string }>('diagram');

	const d = $derived(points.map(([px, py], i) => `${i === 0 ? 'M' : 'L'}${px} ${py}`).join(' '));
	const seg = $derived(points.length - 2);
	const mid = $derived.by(() => {
		const [ax, ay] = points[seg];
		const [bx, by] = points[seg + 1];
		return [(ax + bx) / 2 + labelDx, (ay + by) / 2 + labelDy] as [number, number];
	});
	const stroke = $derived(tone === 'accent' ? '#d97706' : 'currentColor');
	const opacity = $derived(tone === 'muted' ? 0.45 : 1);
	const marker = $derived(
		!arrow ? undefined : tone === 'accent' ? `url(#arr-accent-${uid})` : tone === 'muted' ? `url(#arr-muted-${uid})` : `url(#arr-${uid})`
	);
</script>

<g {opacity}>
	<path {d} fill="none" {stroke} stroke-width={tone === 'accent' ? 1.5 : 1.25} stroke-dasharray={dashed ? '4 3' : undefined} marker-end={marker} />
	{#if label}
		<text x={mid[0]} y={mid[1]} text-anchor="middle" font-size="10.5" fill={stroke} opacity={tone === 'accent' ? 1 : 0.75}>{label}</text>
	{/if}
</g>
