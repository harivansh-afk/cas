<script lang="ts">
	import { Diagram, Node, Edge, Group, Note } from '$lib/components/diagram';
	const reqs = ['r1', 'r2', 'r3', 'r4', 'r5'];
	const x0 = 40;
	const w = 72;
	const gap = 10;
	const xs = reqs.map((_, i) => x0 + i * (w + gap));
</script>

<Diagram
	w={960}
	h={230}
	label="Two rings in guest memory shared with the daemon. The guest posts requests r1 to r5 on the available ring. The daemon has completed r1 to r3 on the used ring, r4 is in flight, r5 not started. When the daemon dies, a replacement sees used equals 3 and, because every completed request was made durable before being marked used, resumes at r4."
>
	<Group x={20} y={10} w={450} h={210} label="shared memory · rings" />
	<Note x={x0} y={48} text="available: the guest posts here" size={10} tone="muted" />
	{#each reqs as r, i (r)}
		<Node x={xs[i]} y={56} w={w} h={36} title={r} tone={i === 3 ? 'accent' : i === 4 ? 'ghost' : 'default'} />
	{/each}
	<Note x={x0} y={128} text="used: I post completions here" size={10} tone="muted" />
	{#each reqs as r, i (r)}
		<Node x={xs[i]} y={136} w={w} h={36} title={i < 3 ? r : ''} sub={i < 3 ? 'durable' : undefined} tone={i < 3 ? 'default' : 'ghost'} />
	{/each}
	<Note x={x0} y={200} text="used = 3" size={10} tone="accent" />

	<Node x={520} y={56} w={190} h={52} title="old daemon" sub="dies with r4 in flight" tone="muted" />
	<Node x={520} y={140} w={190} h={52} title="new daemon" sub="reads used = 3, resumes at r4" tone="accent" />
	<Edge points={[[520, 166], [xs[3] + w, 166]]} tone="accent" />
	<Note x={750} y={62} text={['one request in flight at a time,', 'durable before it is marked used,', 'so used alone is the resume point.']} size={10} tone="muted" />
	<Note x={750} y={150} text={['the real fix: QEMU\'s inflight', 'shared file. our library rejects it.']} size={10} tone="accent" />
</Diagram>
