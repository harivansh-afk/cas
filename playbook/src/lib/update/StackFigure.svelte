<script lang="ts">
	import { Diagram, Node, Edge, Group, Note } from '$lib/components/diagram';
</script>

<Diagram
	w={960}
	h={250}
	label="Three boxes left to right. Guest: an application, the kernel's virtio-blk driver, and the request rings in guest memory. QEMU: the vhost-user-blk frontend, which sets the device up and passes the ring addresses and a guest-memory file descriptor to the daemon over a unix socket, then leaves the data path. Daemon: the vhost-user-backend crate from rust-vmm, which maps guest memory, runs the event loop and speaks the protocol; above it my code, request parsing and the storage worker; then the staging log. The rings connect straight to the daemon: the data path bypasses QEMU."
>
	<Group x={10} y={10} w={330} h={200} label="guest" />
	<Node x={30} y={40} w={120} h={44} title="application" sub="read / write" tone="muted" />
	<Edge points={[[90, 84], [90, 108]]} tone="muted" />
	<Node x={30} y={108} w={120} h={44} title="guest kernel" sub="virtio-blk driver" tone="muted" />
	<Edge points={[[150, 130], [190, 130]]} />
	<Node x={190} y={100} w={130} h={60} title="rings" sub={['avail / used', 'in guest memory']} tone="outline" />
	<Note x={175} y={190} anchor="middle" tone="muted" size={10} text="guest memory is shared with the daemon" />

	<Group x={370} y={10} w={200} h={200} label="QEMU" />
	<Node x={390} y={40} w={160} h={52} title="vhost-user-blk" sub="device frontend" tone="muted" />
	<Note x={470} y={150} anchor="middle" tone="muted" size={10} text={['sets the device up,', 'passes ring addresses and a', 'guest-memory fd over a socket,', 'then leaves the data path']} />

	<Group x={600} y={10} w={350} h={200} label="daemon" tone="accent" />
	<Node x={620} y={40} w={310} h={44} title="vhost-user-backend" sub="rust-vmm · protocol, memory map, event loop" tone="outline" />
	<Edge points={[[775, 84], [775, 104]]} tone="accent" />
	<Node x={620} y={104} w={310} h={44} title="my code" sub="parse request, storage worker, status, used" tone="accent" />
	<Edge points={[[775, 148], [775, 168]]} tone="accent" />
	<Node x={620} y={168} w={310} h={32} title="staging log" tone="accent" />

	<Edge points={[[550, 66], [620, 62]]} tone="muted" dashed />
	<Edge points={[[320, 130], [620, 126]]} tone="accent" label="data path, no QEMU" labelDy={-10} labelDx={0} />
</Diagram>
