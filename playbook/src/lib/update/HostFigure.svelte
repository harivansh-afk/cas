<script lang="ts">
	import { Diagram, Node, Edge, Group, Note } from '$lib/components/diagram';
</script>

<Diagram
	w={960}
	h={200}
	label="One host. Guest, QEMU, the Rust daemon and its staging log are built. The compactor, chunk store and peer protocol are dashed: planned, not implemented."
>
	<Group x={10} y={10} w={600} h={110} label="built" tone="accent" />
	<Node x={30} y={44} w={120} h={48} title="guest" sub="virtio-blk" tone="muted" />
	<Edge points={[[150, 68], [180, 68]]} />
	<Node x={180} y={44} w={120} h={48} title="QEMU" sub="vhost-user-blk" tone="muted" />
	<Edge points={[[300, 68], [330, 68]]} />
	<Node x={330} y={44} w={110} h={48} title="daemon" sub="Rust, io_uring" tone="accent" />
	<Edge points={[[440, 68], [470, 68]]} tone="accent" />
	<Node x={470} y={44} w={120} h={48} title="staging log" sub={['append', 'FLUSH = fdatasync']} tone="accent" />
	<Note x={310} y={150} anchor="middle" tone="muted" text="the hot path hashes nothing" />

	<Group x={660} y={10} w={290} h={180} label="planned" />
	<Edge points={[[590, 68], [680, 68]]} dashed tone="muted" />
	<Node x={680} y={44} w={130} h={48} title="compactor" sub="chunk, hash, place" tone="ghost" />
	<Edge points={[[745, 92], [745, 122]]} dashed tone="muted" />
	<Node x={680} y={122} w={130} h={48} title="chunk store" sub="index, manifests" tone="ghost" />
	<Edge points={[[810, 146], [840, 146]]} dashed tone="muted" />
	<Node x={840} y={122} w={100} h={48} title="peer host" sub="GET / PUT" tone="ghost" />
</Diagram>
