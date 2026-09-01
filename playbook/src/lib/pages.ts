export interface Page {
	num: string;
	title: string;
	description: string;
}

export const repo = 'https://github.com/harivansh-afk/cas';

/** Source of a page on GitHub. */
export const source = (num: string) => `${repo}/blob/main/playbook/src/routes/${num}/+page.svelte`;

export const pages: Page[] = [
	{ num: '00', title: 'Thesis', description: 'Claim, hypotheses, scope' },
	{ num: '01', title: 'Architecture', description: 'Daemon, data paths, protocol, durability' },
	{ num: '02', title: 'One host', description: 'Part 1: daemon against ZFS on one host' },
	{ num: '03', title: 'Multiple hosts', description: 'Part 2: placement and transfer across hosts' },
	{ num: '04', title: 'Remote read', description: 'Part 3: cold read latency by transport' },
	{ num: '05', title: 'Plan', description: 'Hardware, schedule, gates, risks' },
	{ num: '06', title: 'Prior work', description: 'Nearest systems and what this adds' }
];
