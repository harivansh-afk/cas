<script lang="ts">
	import { Diagram, Node, Note } from '$lib/components/diagram';
	const blocks = [0, 1, 2, 3];
	const bw = 64;
	const x0 = 200;
</script>

<Diagram
	w={960}
	h={190}
	label="The same 16 KiB of data with one 4 KiB block changed. Cut as one 16 KiB chunk, the whole chunk is new and 16 KiB is stored again. Cut as four 4 KiB chunks, one chunk is new and 4 KiB is stored again; the other three still match."
>
	<Note x={x0 - 20} y={54} anchor="end" text="one 16 KiB chunk" size={10.5} tone="muted" />
	<Node x={x0} y={30} w={bw * 4} h={40} title="" tone="accent" />
	{#each blocks as b (b)}
		<Node x={x0 + b * bw} y={30} w={bw} h={40} title={b === 2 ? 'changed' : ''} tone={b === 2 ? 'accent' : 'ghost'} />
	{/each}
	<Note x={x0 + bw * 4 + 24} y={54} text="whole chunk new · 16 KiB stored again" size={10.5} tone="accent" />

	<Note x={x0 - 20} y={134} anchor="end" text="four 4 KiB chunks" size={10.5} tone="muted" />
	{#each blocks as b (b)}
		<Node x={x0 + b * bw} y={110} w={bw} h={40} title={b === 2 ? 'changed' : 'same'} tone={b === 2 ? 'accent' : 'default'} />
	{/each}
	<Note x={x0 + bw * 4 + 24} y={134} text="one chunk new · 4 KiB stored again, three still shared" size={10.5} />
	<Note x={x0} y={172} text="the price: one index entry per chunk, about 10 GB of RAM per TB at 4 KiB, a quarter of that at 16 KiB" size={10} tone="muted" />
</Diagram>
