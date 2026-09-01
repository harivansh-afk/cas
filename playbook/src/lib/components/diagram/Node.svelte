<svelte:options namespace="svg" />

<script lang="ts">
	/**
	 * A box in a diagram.
	 * tone:  default  — neutral outline
	 *        accent   — amber outline and tint: the thing the figure is about
	 *        outline  — amber outline, no tint: related to the accent, one step down
	 *        muted    — faded: present for completeness, not the point
	 *        ghost    — dashed outline: a boundary, a decision, or something not yet real
	 * kind:  box | question (pill, dashed, for decision points)
	 */
	let {
		x,
		y,
		w,
		h = 44,
		title,
		sub,
		tone = 'default',
		kind = 'box'
	}: {
		x: number;
		y: number;
		w: number;
		h?: number;
		title: string;
		sub?: string | string[];
		tone?: 'default' | 'accent' | 'outline' | 'muted' | 'ghost';
		kind?: 'box' | 'question';
	} = $props();

	const subs = $derived(Array.isArray(sub) ? sub : sub ? [sub] : []);
	const cx = $derived(x + w / 2);
	const lines = $derived(1 + subs.length);
	const lineH = 15;
	const firstY = $derived(y + h / 2 - ((lines - 1) * lineH) / 2 + 4);

	const stroke = $derived(tone === 'accent' || tone === 'outline' ? '#d97706' : 'currentColor');
	const strokeW = $derived(tone === 'accent' ? 1.5 : tone === 'muted' ? 1 : 1.25);
	const fill = $derived(tone === 'accent' ? '#d97706' : 'none');
	const fillOpacity = $derived(tone === 'accent' ? 0.14 : 0);
	const opacity = $derived(tone === 'muted' ? 0.5 : tone === 'ghost' ? 0.7 : 1);
	const dash = $derived(tone === 'ghost' || kind === 'question' ? '4 3' : undefined);
	const rx = $derived(kind === 'question' ? h / 2 : 4);
	const weight = $derived(tone === 'muted' || kind === 'question' ? 500 : 600);
</script>

<g {opacity}>
	<rect {x} {y} width={w} height={h} {rx} {fill} fill-opacity={fillOpacity} {stroke} stroke-width={strokeW} stroke-dasharray={dash} />
	<text x={cx} y={firstY} text-anchor="middle" font-size={kind === 'question' ? 10.5 : 11.5} font-weight={weight} fill="currentColor">{title}</text>
	{#each subs as s, i (i)}
		<text x={cx} y={firstY + (i + 1) * lineH} text-anchor="middle" font-size="10.5" fill={tone === 'accent' ? '#d97706' : 'currentColor'} opacity={tone === 'accent' ? 1 : 0.6}>{s}</text>
	{/each}
</g>
