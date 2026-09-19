/**
 * The whole system as one graph. Nodes nest through `parent`; edges connect
 * any two nodes and are lifted to whatever is visible. Text on a node is what
 * the diagram shows; the rest is what the detail panel shows when it is chosen.
 */

import { machine } from './parts/machine';
import { host } from './parts/host';
import { core } from './parts/core';
import { tooling } from './parts/tooling';
import { edges as allEdges } from './edges';
import { flows as allFlows } from './flows';

export type Tone = 'accent' | 'outline' | 'muted' | 'ghost';

export interface SystemNode {
	id: string;
	title: string;
	sub?: string;
	parent?: string;
	tone?: Tone;
	/** One paragraph: what this is and what it is for. */
	what: string;
	/** How it works, step by step or point by point. */
	how?: string[];
	/** Rust types, functions or files that carry it, as written in the source. */
	types?: string[];
	/** Repository paths, pinned to the revision on the page. */
	files?: string[];
	/** Design notes under docs/, by name without `.md`. */
	docs?: string[];
	/** Constants that matter, with where they are set. */
	numbers?: [string, string][];
}

export interface SystemEdge {
	from: string;
	to: string;
	label?: string;
	dashed?: boolean;
}

export interface FlowStep {
	node: string;
	/** The edge this step travels, as [from, to] of any two nodes. */
	via?: [string, string];
	text: string;
}

export interface Flow {
	id: string;
	name: string;
	result: string;
	steps: FlowStep[];
}

export const nodes: SystemNode[] = [...machine, ...host, ...core, ...tooling];
export const edges: SystemEdge[] = allEdges;
export const flows: Flow[] = allFlows;

/** Nodes expanded when the page first renders: the two processes that matter. */
export const initiallyExpanded: string[] = ['host', 'core'];

/** Every id an edge or flow names must exist; checked once at module load. */
{
	const ids = new Set(nodes.map((n) => n.id));
	for (const n of nodes) if (n.parent && !ids.has(n.parent)) throw new Error(`unknown parent ${n.parent} on ${n.id}`);
	for (const e of edges) for (const id of [e.from, e.to]) if (!ids.has(id)) throw new Error(`edge names unknown node ${id}`);
	for (const f of flows) for (const s of f.steps) for (const id of [s.node, ...(s.via ?? [])]) if (!ids.has(id)) throw new Error(`flow ${f.id} names unknown node ${id}`);
}
