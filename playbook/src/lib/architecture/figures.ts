type Node = { id: string; x: number; y: number; w: number; h?: number; title: string; sub?: string; tone?: 'accent' | 'muted' };
type Edge = { from: string; to: string; points: [number, number][]; label?: string; labelDx?: number; labelDy?: number; dashed?: boolean };
type Group = { id: string; x: number; y: number; w: number; h: number; label: string; members: string[] };
type Figure = { title: string; caption: string; h: number; nodes: Node[]; edges: Edge[]; groups?: Group[] };

export const figures = {
	memory: {
		title: 'Three different things called shared memory', h: 406,
		caption: 'The guest-RAM mapping, retained recovery carrier and host io_uring mappings have different contents and lifetimes. None forwards a guest syscall.',
		nodes: [
			{ id: 'guest', x: 20, y: 20, w: 210, title: 'Guest driver', sub: 'posts head; reads status' },
			{ id: 'ram', x: 295, y: 20, w: 230, h: 65, title: 'Shared guest RAM', sub: 'descriptors / avail / used / data', tone: 'accent' },
			{ id: 'backend', x: 590, y: 20, w: 190, title: 'CAS frontend', sub: 'maps same backing pages' },
			{ id: 'qemu', x: 20, y: 150, w: 210, title: 'QEMU', sub: 'retains the carrier FD' },
			{ id: 'carrier', x: 295, y: 140, w: 230, h: 65, title: 'Inflight memfd', sub: 'identities + P; no payload', tone: 'accent' },
			{ id: 'replacement', x: 590, y: 150, w: 190, title: 'Replacement CAS', sub: 'reconcile + replay' },
			{ id: 'reactor', x: 20, y: 290, w: 210, title: 'CAS reactor', sub: 'host FDs + aligned buffers' },
			{ id: 'uring', x: 295, y: 280, w: 230, h: 65, title: 'Host io_uring', sub: 'SQEs / CQEs; no guest rings', tone: 'accent' },
			{ id: 'kernel', x: 590, y: 290, w: 190, title: 'Host kernel', sub: 'executes storage IO' }
		],
		edges: [
			{ from: 'guest', to: 'ram', points: [[230,42],[295,42]] },
			{ from: 'ram', to: 'backend', points: [[525,42],[590,42]] },
			{ from: 'qemu', to: 'carrier', points: [[230,172],[295,172]] },
			{ from: 'carrier', to: 'replacement', points: [[525,172],[590,172]] },
			{ from: 'reactor', to: 'uring', points: [[230,312],[295,312]] },
			{ from: 'uring', to: 'kernel', points: [[525,312],[590,312]] }
		]
	},
	owners: {
		title: 'Private guest disks, one shared CAS process', h: 552,
		caption: 'QEMU sets up each disk over a Unix socket. Requests use shared guest RAM and eventfds. Inside cas-host, each image keeps its own write order; the shared compactor publishes chunks and manifests.',
		groups: [{ id: 'process', x: 10, y: 130, w: 780, h: 406, label: 'cas-host process · cas-daemon crate', members: ['a','b','ra','rb','shared','worker'] }],
		nodes: [
			{ id: 'ga', x: 30, y: 10, w: 220, h: 58, title: 'QEMU + guest A', sub: 'private filesystem / virtqueues' },
			{ id: 'gb', x: 550, y: 10, w: 220, h: 58, title: 'QEMU + guest B', sub: 'private filesystem / virtqueues' },
			{ id: 'a', x: 30, y: 175, w: 220, title: 'Image A frontend', sub: 'socket / parser / completion' },
			{ id: 'b', x: 550, y: 175, w: 220, title: 'Image B frontend', sub: 'socket / parser / completion' },
			{ id: 'ra', x: 30, y: 283, w: 220, title: 'Image A reactor', sub: 'private WAL + manifest view', tone: 'accent' },
			{ id: 'rb', x: 550, y: 283, w: 220, title: 'Image B reactor', sub: 'private WAL + manifest view', tone: 'accent' },
			{ id: 'worker', x: 285, y: 275, w: 230, h: 60, title: 'Compactor thread', sub: 'chunks / manifests / catalog', tone: 'accent' },
			{ id: 'shared', x: 250, y: 435, w: 300, h: 58, title: 'Shared host resources', sub: 'index / caches / budgets / gate' }
		],
		edges: [
			{ from: 'ga', to: 'a', points: [[140,68],[140,175]], label: 'guest RAM + eventfd', labelDx: 103 },
			{ from: 'gb', to: 'b', points: [[660,68],[660,175]], label: 'guest RAM + eventfd', labelDx: -103 },
			{ from: 'a', to: 'ra', points: [[140,219],[140,283]], label: 'channel + eventfd', labelDx: 82 },
			{ from: 'b', to: 'rb', points: [[660,219],[660,283]], label: 'channel + eventfd', labelDx: -82 },
			{ from: 'ra', to: 'worker', points: [[250,305],[285,305]] },
			{ from: 'rb', to: 'worker', points: [[550,305],[515,305]] },
			{ from: 'ra', to: 'shared', points: [[140,327],[140,464],[250,464]] },
			{ from: 'rb', to: 'shared', points: [[660,327],[660,464],[550,464]] },
			{ from: 'worker', to: 'shared', points: [[400,335],[400,435]], label: 'in-process access', labelDx: 92 }
		]
	},
	compact: {
		title: 'Compaction publishes data before the mapping that names it', h: 306,
		caption: 'A bounded, durable prefix is copied into the shared content store. Reclamation follows the durable manifest and the retirement of readers and replay identities.',
		nodes: [
			{ id: 'wal', x: 15, y: 25, w: 225, h: 58, title: 'Durable WAL prefix', sub: 'D < sequence ≤ E' },
			{ id: 'hash', x: 285, y: 25, w: 225, h: 58, title: 'Surviving 4 KiB blocks', sub: 'verify CRC → BLAKE3', tone: 'accent' },
			{ id: 'store', x: 555, y: 25, w: 230, h: 58, title: 'Missing chunks → sync', sub: 'reuse existing hashes', tone: 'accent' },
			{ id: 'manifest', x: 555, y: 185, w: 230, h: 58, title: 'COW pages + COMMIT', sub: 'sync manifest file' },
			{ id: 'publish', x: 285, y: 185, w: 225, h: 58, title: 'Publish new View + D', sub: 'under completion gate', tone: 'accent' },
			{ id: 'reclaim', x: 15, y: 185, w: 225, h: 58, title: 'Reclaim staging', sub: 'wait for read / identity pins' }
		],
		edges: [
			{ from: 'wal', to: 'hash', points: [[240,54],[285,54]] },
			{ from: 'hash', to: 'store', points: [[510,54],[555,54]] },
			{ from: 'store', to: 'manifest', points: [[670,83],[670,185]], label: 'data is durable first', labelDx: -100 },
			{ from: 'manifest', to: 'publish', points: [[555,214],[510,214]] },
			{ from: 'publish', to: 'reclaim', points: [[285,214],[240,214]] }
		]
	},
	lookup: {
		title: 'A read overlays recent mutations on a committed manifest', h: 438,
		caption: 'Staging has precedence, including an explicit ZERO. Only uncovered manifest hashes reach the shared cache and chunk store.',
		nodes: [
			{ id: 'read', x: 20, y: 25, w: 215, title: 'READ logical range', sub: 'wait for captured boundary' },
			{ id: 'staging', x: 295, y: 25, w: 220, title: 'Staging interval index', sub: 'newest published mutations', tone: 'accent' },
			{ id: 'payload', x: 575, y: 25, w: 210, title: 'Pinned WAL or ZERO', sub: 'CRC-checked data / zeros' },
			{ id: 'view', x: 295, y: 145, w: 220, title: 'Captured manifest View', sub: 'B+tree pages → hash / hole' },
			{ id: 'pages', x: 20, y: 145, w: 215, title: 'Manifest page cache', sub: 'or verified direct page IO' },
			{ id: 'cache', x: 295, y: 255, w: 220, title: 'Shared chunk LRU', sub: 'full 32-byte hash key', tone: 'accent' },
			{ id: 'response', x: 575, y: 255, w: 210, title: 'Owned response', sub: 'copy back to guest RAM' },
			{ id: 'fetch', x: 20, y: 360, w: 250, title: 'Coalesced hash fetch', sub: 'one leader; bounded waiters' },
			{ id: 'disk', x: 325, y: 360, w: 300, title: 'Hash index → segment + block', sub: 'verify header, CRC and BLAKE3' }
		],
		edges: [
			{ from: 'read', to: 'staging', points: [[235,47],[295,47]] },
			{ from: 'staging', to: 'payload', points: [[515,47],[575,47]], label: 'covered' },
			{ from: 'staging', to: 'view', points: [[405,69],[405,145]], label: 'uncovered', labelDx: 65 },
			{ from: 'pages', to: 'view', points: [[235,167],[295,167]] },
			{ from: 'view', to: 'cache', points: [[405,189],[405,255]], label: 'hash', labelDx: 32 },
			{ from: 'cache', to: 'response', points: [[515,277],[575,277]], label: 'hit' },
			{ from: 'payload', to: 'response', points: [[680,69],[680,255]] },
			{ from: 'cache', to: 'fetch', points: [[295,277],[145,277],[145,360]], label: 'miss', labelDx: -27 },
			{ from: 'fetch', to: 'disk', points: [[270,382],[325,382]] },
			{ from: 'disk', to: 'response', points: [[625,382],[730,382],[730,299]] }
		]
	},
	recovery: {
		title: 'A retained reconnect keeps QEMU alive while replacing cas-host', h: 365,
		caption: 'Every image contributes its original memory, queue state and inflight carrier before the shared recovery barrier can open.',
		nodes: [
			{ id: 'qemu', x: 20, y: 20, w: 230, h: 60, title: 'QEMU A + QEMU B survive', sub: 'guest RAM + carrier FDs remain' },
			{ id: 'old', x: 295, y: 20, w: 225, h: 60, title: 'Old cas-host killed', sub: 'old IO owners must release' },
			{ id: 'new', x: 565, y: 20, w: 220, h: 60, title: 'New cas-host: retained', sub: 'lock + inspect all images', tone: 'accent' },
			{ id: 'validate', x: 565, y: 150, w: 220, h: 60, title: 'Validate every carrier', sub: 'prefix P + original identity' },
			{ id: 'replay', x: 295, y: 150, w: 225, h: 60, title: 'Replay missing mutations', sub: 'original per-image sequences', tone: 'accent' },
			{ id: 'fence', x: 20, y: 150, w: 230, h: 60, title: 'Recovery FENCE + sync', sub: 'for every catalog image' },
			{ id: 'resume', x: 20, y: 285, w: 230, title: 'Restore completions', sub: 'then enable ordinary admission' }
		],
		edges: [
			{ from: 'old', to: 'new', points: [[520,50],[565,50]] },
			{ from: 'qemu', to: 'validate', points: [[135,80],[135,113],[675,113],[675,150]], dashed: true },
			{ from: 'new', to: 'validate', points: [[750,80],[750,150]] },
			{ from: 'validate', to: 'replay', points: [[565,180],[520,180]] },
			{ from: 'replay', to: 'fence', points: [[295,180],[250,180]] },
			{ from: 'fence', to: 'resume', points: [[135,210],[135,285]], label: 'shared barrier', labelDx: 89 }
		]
	}
} satisfies Record<string, Figure>;

export type FigureName = keyof typeof figures;

// The readable diagram markup is derived from the same nodes and edges as the SVG.
// Coordinates affect the inline drawing; Mermaid chooses its own layout.
export function mermaid(figure: Figure): string {
	const label = (node: Node) => `${node.title}${node.sub ? ` — ${node.sub}` : ''}`.replaceAll('"', '&quot;');
	const lines = ['flowchart TB'];
	for (const group of figure.groups ?? []) {
		lines.push(`  subgraph ${group.id}["${group.label}"]`);
		for (const id of group.members) {
			const node = figure.nodes.find((node) => node.id === id)!;
			lines.push(`    ${id}["${label(node)}"]`);
		}
		lines.push('  end');
	}
	for (const node of figure.nodes) {
		if (!figure.groups?.some((group) => group.members.includes(node.id))) lines.push(`  ${node.id}["${label(node)}"]`);
	}
	for (const edge of figure.edges) lines.push(`  ${edge.from} ${edge.dashed ? '-.->' : '-->'}${edge.label ? `|"${edge.label}"|` : ''} ${edge.to}`);
	return lines.join('\n');
}
