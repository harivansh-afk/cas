<script lang="ts">
	import { Diagram, Node, Edge, Note } from '$lib/components/diagram';
	// One staging log: header, then fixed 8 KiB slots. Fences are metadata-only slots.
	const slots: { title: string; sub: string; tone: 'default' | 'accent' | 'muted' | 'ghost' }[] = [
		{ title: 'W 1', sub: 'crc ok', tone: 'default' },
		{ title: 'W 2', sub: 'crc ok', tone: 'default' },
		{ title: 'F 2', sub: 'fence', tone: 'accent' },
		{ title: 'W 3', sub: 'crc ok', tone: 'default' },
		{ title: 'W 4', sub: 'crc ok', tone: 'default' },
		{ title: 'F 4', sub: 'fence', tone: 'accent' },
		{ title: 'W 5', sub: 'torn', tone: 'ghost' },
		{ title: '…', sub: 'zeros', tone: 'ghost' }
	];
	const x0 = 130;
	const w = 92;
	const gap = 8;
	const y = 70;
	const xs = slots.map((_, i) => x0 + i * (w + gap));
</script>

<Diagram
	w={960}
	h={230}
	label="A staging log as fixed 8 KiB slots after a header: writes 1 and 2, a fence at sequence 2, writes 3 and 4, a fence at sequence 4, then a torn write 5 and a zero-filled tail. Recovery scans backwards to the last valid fence, replays forward through it, and cuts everything after it."
>
	<Node x={20} y={y} w={100} h={48} title="header" sub="4 KiB" tone="muted" />
	{#each slots as s, i (i)}
		<Node x={xs[i]} y={y} w={w} h={48} title={s.title} sub={s.sub} tone={s.tone} />
	{/each}
	<Note x={xs[0]} y={y - 22} text="each slot: 4 KiB metadata + 4 KiB data, one CRC32" size={10} tone="muted" />

	<Edge points={[[xs[7] + w / 2, y + 70], [xs[5] + w / 2, y + 70]]} tone="accent" label="1 · scan back to the last valid fence" labelDy={22} />
	<Edge points={[[xs[0], y + 100], [xs[5] + w / 2, y + 100]]} tone="muted" label="2 · replay forward, checking every CRC" labelDy={22} />
	<Note x={xs[6] + w + gap / 2} y={y + 148} anchor="middle" tone="accent" text="3 · cut" size={10} />
	<Note x={xs[5] + w / 2} y={y - 22} anchor="middle" tone="accent" text="E = 4" size={10} />
</Diagram>
