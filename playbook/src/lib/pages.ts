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
	{ num: '02', title: 'Single host', description: 'The daemon against ZFS on one host' },
	{ num: '03', title: 'Multiple hosts', description: 'Placement and transfer across hosts' },
	{ num: '04', title: 'Remote read', description: 'Cold read latency by transport' },
	{ num: '05', title: 'Plan', description: 'Hardware, schedule, gates, risks' },
	{ num: '06', title: 'Prior art', description: 'Nearest systems and what this adds' }
];
