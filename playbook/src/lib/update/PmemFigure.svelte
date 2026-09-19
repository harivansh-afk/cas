<script lang="ts">
	import { Diagram, Node, Edge, Group, Note } from '$lib/components/diagram';
	const guests = ['guest 1', 'guest 2', 'guest 3'];
</script>

<Diagram
	w={960}
	h={230}
	label="Left, virtio-blk today: three guests each hold their own copy of the base image in their page cache. Right, virtio-pmem with DAX: the host maps the image once and all three guests read the same pages."
>
	<Group x={10} y={10} w={450} h={210} label="today · virtio-blk" />
	{#each guests as g, i (g)}
		<Node x={30 + i * 145} y={40} w={135} h={48} title={g} sub="own copy" />
		<Edge points={[[97 + i * 145, 150], [97 + i * 145, 88]]} tone="muted" />
	{/each}
	<Node x={30} y={150} w={420} h={44} title="base image on NVMe" tone="muted" />
	<Note x={235} y={212} anchor="middle" tone="muted" text="three guests, three copies in memory" />

	<Group x={500} y={10} w={450} h={210} label="proposed · virtio-pmem + DAX" tone="accent" />
	{#each guests as g, i (g)}
		<Node x={520 + i * 145} y={40} w={135} h={48} title={g} sub="shared pages" tone="outline" />
		<Edge points={[[587 + i * 145, 150], [587 + i * 145, 88]]} tone="accent" />
	{/each}
	<Node x={520} y={150} w={420} h={44} title="base image mapped once, read-only" tone="accent" />
	<Note x={725} y={212} anchor="middle" tone="accent" text="three guests, one copy in memory" />
</Diagram>
