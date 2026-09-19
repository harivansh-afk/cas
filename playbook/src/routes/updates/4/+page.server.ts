import { nodes, edges, initiallyExpanded } from '$lib/system/graph';
import { layoutSystem } from '$lib/system/layout';
import type { PageServerLoad } from './$types';

export const prerender = true;

/** The starting view is laid out at build time, so the page is complete without JavaScript. */
export const load: PageServerLoad = async () => {
	const initial = await layoutSystem(nodes, edges, new Set(initiallyExpanded));
	return { initial };
};
