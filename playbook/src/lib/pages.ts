export interface Page {
	num: string;
	title: string;
	description: string;
}

export const repo = 'https://github.com/harivansh-afk/cas';

/** Source of a page on GitHub. */
export const source = (num: string) => `${repo}/blob/main/playbook/src/routes/${num}/+page.svelte`;

export const pages: Page[] = [
	{ num: '00', title: 'Thesis', description: 'Where dedup stops · what a name buys · H1–H3 · scope' },
	{ num: '01', title: 'Architecture', description: 'One daemon per host · local write · global chunks · protocol · durability' },
	{ num: '02', title: 'One host', description: 'Part 1 · daemon vs ZFS fast dedup · chunk size vs index · prediction' },
	{ num: '03', title: 'Two hosts', description: 'Part 2 · replicated vs partitioned · provision, migrate, sync · the window' },
	{ num: '04', title: 'Remote read', description: 'Part 3 · peer RAM vs local NVMe · TCP vs RDMA · prefetch' },
	{ num: '05', title: 'Plan', description: 'Hardware · 14 weeks · gates · cut order · risks' },
	{ num: '06', title: 'Prior work', description: 'Datrium, Nutanix, Venti, TiDedup, DADI · what remains' }
];
