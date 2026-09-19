import type { SystemEdge, SystemNode } from './graph';

/**
 * Lays out the system graph for one expansion state.
 *
 * Every node has an optional parent. A node is visible when all its ancestors
 * are expanded. An expanded node with children is drawn as a cluster around
 * them; a collapsed one is drawn as a single box. Edges are defined between
 * any two nodes and lifted to the nearest visible ancestor of each endpoint,
 * so a collapsed box keeps every connection its members have.
 *
 * ELK's layered algorithm (the engine behind Mermaid's subgraph layouts)
 * chooses layers, positions and orthogonal edge routes; the coordinates are
 * only for the inline drawing.
 */

export interface Box {
	id: string;
	x: number; // top-left, absolute
	y: number;
	w: number;
	h: number;
	node: SystemNode;
	/** Number of hidden descendants when collapsed; 0 when a leaf or expanded. */
	hidden: number;
	cluster: boolean;
	depth: number;
}

export interface Line {
	id: string;
	from: string;
	to: string;
	points: { x: number; y: number }[];
	label?: string;
	labelAt?: { x: number; y: number; w: number; h: number };
	dashed: boolean;
	/** The original edges this line stands for, so flows can highlight it. */
	members: SystemEdge[];
}

export interface Layout {
	w: number;
	h: number;
	boxes: Box[];
	lines: Line[];
}

const TITLE_CH = 6.95; // Berkeley Mono at 11.5px
const SUB_CH = 6.35; // at 10.5px
const LABEL_CH = 6.05; // at 10px
const BOX_H = 46;
const PAD = 26;
const SEP = '\u0000';

export function childrenOf(nodes: SystemNode[]): Map<string, SystemNode[]> {
	const map = new Map<string, SystemNode[]>();
	for (const node of nodes) {
		if (!node.parent) continue;
		const list = map.get(node.parent) ?? [];
		list.push(node);
		map.set(node.parent, list);
	}
	return map;
}

export function ancestorsOf(id: string, byId: Map<string, SystemNode>): string[] {
	const out: string[] = [];
	let cur = byId.get(id)?.parent;
	while (cur) {
		out.unshift(cur);
		cur = byId.get(cur)?.parent;
	}
	return out;
}

export function descendantCount(id: string, children: Map<string, SystemNode[]>): number {
	let n = 0;
	for (const child of children.get(id) ?? []) n += 1 + descendantCount(child.id, children);
	return n;
}

function boxSize(node: SystemNode, hidden: number): { w: number; h: number } {
	const badge = hidden ? 36 : 0;
	const title = node.title.length * TITLE_CH + badge;
	const sub = (node.sub?.length ?? 0) * SUB_CH + badge;
	const w = Math.max(120, Math.ceil(Math.max(title, sub) + PAD));
	const h = node.sub ? BOX_H + 14 : BOX_H;
	return { w, h };
}

export interface Merged {
	from: string;
	to: string;
	labels: string[];
	dashed: boolean;
	members: SystemEdge[];
}

/** The visible lines for one expansion: lifted, merged, with ancestor edges dropped. */
export function visibleEdges(nodes: SystemNode[], edges: SystemEdge[], expanded: ReadonlySet<string>): Map<string, Merged> {
	const byId = new Map(nodes.map((n) => [n.id, n]));
	const visible = (id: string): boolean => ancestorsOf(id, byId).every((a) => expanded.has(a));
	const lift = (id: string): string => {
		let cur: string | undefined = id;
		while (cur && !visible(cur)) cur = byId.get(cur)?.parent;
		return cur ?? id;
	};
	const isAncestor = (a: string, b: string) => ancestorsOf(b, byId).includes(a);
	const merged = new Map<string, Merged>();
	for (const edge of edges) {
		const from = lift(edge.from);
		const to = lift(edge.to);
		if (from === to || isAncestor(from, to) || isAncestor(to, from)) continue;
		const key = from + SEP + to;
		const reverse = merged.get(to + SEP + from);
		if (reverse) {
			// Two directions between one visible pair become one line.
			reverse.members.push(edge);
			if (edge.label && !reverse.labels.includes(edge.label)) reverse.labels.push(edge.label);
			continue;
		}
		const cur = merged.get(key);
		if (cur) {
			cur.members.push(edge);
			if (edge.label && !cur.labels.includes(edge.label)) cur.labels.push(edge.label);
			cur.dashed = cur.dashed && !!edge.dashed;
		} else {
			merged.set(key, { from, to, labels: edge.label ? [edge.label] : [], dashed: !!edge.dashed, members: [edge] });
		}
	}
	return merged;
}

type ElkNode = {
	id: string;
	width?: number;
	height?: number;
	x?: number;
	y?: number;
	children?: ElkNode[];
	layoutOptions?: Record<string, string>;
};
type ElkPoint = { x: number; y: number };
type ElkEdge = {
	id: string;
	sources: string[];
	targets: string[];
	container?: string;
	labels?: { text: string; width: number; height: number; x?: number; y?: number }[];
	sections?: { startPoint: ElkPoint; endPoint: ElkPoint; bendPoints?: ElkPoint[] }[];
};
type ElkGraph = ElkNode & { edges?: ElkEdge[] };
type Elk = { layout(graph: ElkGraph): Promise<ElkGraph> };

let elk: Promise<Elk> | undefined;
async function engine(): Promise<Elk> {
	if (!elk) {
		elk = import('elkjs/lib/elk.bundled.js').then((m) => {
			const Ctor = (m.default ?? m) as unknown as new () => Elk;
			return new Ctor();
		});
	}
	return elk;
}

export const options: Record<string, string> = {
	'elk.algorithm': 'layered',
	'elk.direction': 'DOWN',
	'elk.hierarchyHandling': 'INCLUDE_CHILDREN',
	'elk.edgeRouting': 'ORTHOGONAL',
	'elk.layered.spacing.nodeNodeBetweenLayers': '36',
	'elk.spacing.nodeNode': '22',
	'elk.spacing.edgeNode': '14',
	'elk.spacing.edgeEdge': '10',
	'elk.layered.spacing.edgeNodeBetweenLayers': '14',
	'elk.layered.spacing.edgeEdgeBetweenLayers': '10',
	'elk.layered.considerModelOrder.strategy': 'NODES_AND_EDGES',
	'elk.layered.nodePlacement.strategy': 'BRANDES_KOEPF',
	'elk.layered.nodePlacement.bk.fixedAlignment': 'BALANCED',
	'elk.layered.thoroughness': '10',
	'elk.padding': '[top=12,left=12,bottom=12,right=12]'
};

export async function layoutSystem(nodes: SystemNode[], edges: SystemEdge[], expanded: ReadonlySet<string>, override: Record<string, string> = {}): Promise<Layout> {
	const byId = new Map(nodes.map((n) => [n.id, n]));
	const children = childrenOf(nodes);
	const visible = (id: string): boolean => ancestorsOf(id, byId).every((a) => expanded.has(a));
	const isCluster = (id: string): boolean => expanded.has(id) && (children.get(id)?.length ?? 0) > 0;

	const build = (list: SystemNode[]): ElkNode[] =>
		list
			.filter((n) => visible(n.id))
			.map((node) => {
				if (isCluster(node.id)) {
					return {
						id: node.id,
						layoutOptions: { 'elk.padding': '[top=34,left=14,bottom=14,right=14]' },
						children: build(children.get(node.id) ?? [])
					};
				}
				const { w, h } = boxSize(node, descendantCount(node.id, children));
				return { id: node.id, width: w, height: h };
			});

	const merged = visibleEdges(nodes, edges, expanded);
	const elkEdges: ElkEdge[] = [];
	for (const [key, m] of merged) {
		const label = m.labels.slice(0, 2).join(' · ');
		// Labels are not part of the layout: they are shown on demand beside the
		// middle of a line, so the drawing stays compact.
		elkEdges.push({ id: key, sources: [m.from], targets: [m.to], labels: label && override['x.labels'] === 'layout' ? [{ text: label, width: label.length * LABEL_CH + 8, height: 14 }] : undefined });
	}

	const graph: ElkGraph = {
		id: 'root',
		layoutOptions: { ...options, ...override },
		children: build(nodes.filter((n) => !n.parent)),
		edges: elkEdges
	};

	const out = await (await engine()).layout(graph);

	// Absolute positions: ELK reports each node relative to its parent.
	const abs = new Map<string, { x: number; y: number; w: number; h: number }>();
	const walk = (list: ElkNode[] | undefined, ox: number, oy: number) => {
		for (const n of list ?? []) {
			const x = ox + (n.x ?? 0);
			const y = oy + (n.y ?? 0);
			abs.set(n.id, { x, y, w: n.width ?? 0, h: n.height ?? 0 });
			walk(n.children, x, y);
		}
	};
	walk(out.children, 0, 0);

	const boxes: Box[] = [];
	for (const node of nodes) {
		const p = abs.get(node.id);
		if (!p) continue;
		const cluster = isCluster(node.id);
		boxes.push({ id: node.id, x: p.x, y: p.y, w: p.w, h: p.h, node, hidden: cluster ? 0 : descendantCount(node.id, children), cluster, depth: ancestorsOf(node.id, byId).length });
	}
	boxes.sort((a, b) => (a.cluster === b.cluster ? a.depth - b.depth : a.cluster ? -1 : 1));

	const lines: Line[] = [];
	for (const e of out.edges ?? []) {
		const m = merged.get(e.id);
		if (!m) continue;
		const origin = e.container && e.container !== 'root' ? abs.get(e.container) : undefined;
		const ox = origin?.x ?? 0;
		const oy = origin?.y ?? 0;
		const points: ElkPoint[] = [];
		for (const s of e.sections ?? []) {
			const seq = [s.startPoint, ...(s.bendPoints ?? []), s.endPoint];
			for (const p of seq) points.push({ x: p.x + ox, y: p.y + oy });
		}
		const label = m.labels.slice(0, 2).join(' · ') || undefined;
		lines.push({ id: e.id, from: m.from, to: m.to, points, label, labelAt: label ? labelSpot(points, label) : undefined, dashed: m.dashed, members: m.members });
	}

	return { w: Math.ceil(out.width ?? 0), h: Math.ceil(out.height ?? 0), boxes, lines };
}

/** Where a label sits: centred on the longest segment of the line. */
function labelSpot(points: { x: number; y: number }[], label: string): { x: number; y: number; w: number; h: number } {
	let best = 0;
	let bestLen = -1;
	for (let i = 0; i < points.length - 1; i++) {
		const len = Math.hypot(points[i + 1].x - points[i].x, points[i + 1].y - points[i].y);
		if (len > bestLen) {
			bestLen = len;
			best = i;
		}
	}
	const a = points[best];
	const b = points[best + 1];
	const w = label.length * LABEL_CH + 8;
	const h = 14;
	return { x: (a.x + b.x) / 2 - w / 2, y: (a.y + b.y) / 2 - h / 2, w, h };
}

/** An orthogonal path through ELK's points with small rounded corners. */
export function pathFor(points: { x: number; y: number }[]): string {
	if (points.length < 2) return '';
	const r = 6;
	let d = `M${points[0].x} ${points[0].y}`;
	for (let i = 1; i < points.length - 1; i++) {
		const p = points[i - 1];
		const c = points[i];
		const n = points[i + 1];
		const din = Math.hypot(c.x - p.x, c.y - p.y);
		const dout = Math.hypot(n.x - c.x, n.y - c.y);
		const k = Math.min(r, din / 2, dout / 2);
		if (k < 1) {
			d += ` L${c.x} ${c.y}`;
			continue;
		}
		const ax = c.x - ((c.x - p.x) / din) * k;
		const ay = c.y - ((c.y - p.y) / din) * k;
		const bx = c.x + ((n.x - c.x) / dout) * k;
		const by = c.y + ((n.y - c.y) / dout) * k;
		d += ` L${ax} ${ay} Q${c.x} ${c.y} ${bx} ${by}`;
	}
	const last = points[points.length - 1];
	d += ` L${last.x} ${last.y}`;
	return d;
}

/** Mermaid markup for the current expansion, derived from the same nodes and edges. */
export function mermaidFor(nodes: SystemNode[], edges: SystemEdge[], expanded: ReadonlySet<string>): string {
	const byId = new Map(nodes.map((n) => [n.id, n]));
	const children = childrenOf(nodes);
	const visible = (id: string): boolean => ancestorsOf(id, byId).every((a) => expanded.has(a));
	const isCluster = (id: string): boolean => expanded.has(id) && (children.get(id)?.length ?? 0) > 0;
	const q = (s: string) => s.replaceAll('"', '&quot;');
	const lines = ['flowchart TB'];
	const emit = (id: string, indent: string) => {
		const node = byId.get(id)!;
		if (isCluster(id)) {
			lines.push(`${indent}subgraph ${id}["${q(node.title)}"]`);
			for (const child of children.get(id) ?? []) emit(child.id, indent + '  ');
			lines.push(`${indent}end`);
		} else {
			lines.push(`${indent}${id}["${q(node.title)}${node.sub ? ` — ${q(node.sub)}` : ''}"]`);
		}
	};
	for (const node of nodes) if (!node.parent && visible(node.id)) emit(node.id, '  ');
	for (const m of visibleEdges(nodes, edges, expanded).values()) {
		const label = m.labels.slice(0, 2).join(' · ');
		lines.push(`  ${m.from} ${m.dashed ? '-.->' : '-->'}${label ? `|"${q(label)}"|` : ''} ${m.to}`);
	}
	return lines.join('\n');
}
