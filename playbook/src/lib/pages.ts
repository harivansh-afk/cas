export interface Page {
	num: string;
	title: string;
	description: string;
	/** Not part of the registered study; listed greyed out on the index. */
	draft?: boolean;
}

export const repo = 'https://github.com/harivansh-afk/cas';

/** Source of a page on GitHub. */
export const source = (num: string) => `${repo}/blob/main/playbook/src/routes/${num}/+page.svelte`;

export const pages: Page[] = [
	{ num: '00', title: 'Thesis', description: 'The split nobody has measured · prior art · H1, H2 · what the study proves · assumptions' },
	{ num: '01', title: 'Census', description: 'Phase 1 · real fleets first · four-leaf decomposition · the curve · the week-6 gate' },
	{ num: '02', title: 'Cost on stock systems', description: 'Phase 2 · XFS, ZFS fast dedup, dm-vdo, duperemove · transfer and cache as headline' },
	{ num: '03', title: 'The CDC instrument', description: 'Phase 3, conditional · a two-tier backend built only if the census opens the gate' },
	{ num: '04', title: 'Plan', description: 'Hardware · schedule · gates G1–G5 · cut order · risks · what comes out' },
	{ num: '05', title: 'KV-cache', description: 'The same split in LLM serving · prefix caching as lineage · the census Irminsul ran · what is still open', draft: true }
];
