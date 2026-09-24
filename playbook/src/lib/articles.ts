/** Long-form meeting updates rendered as articles; decks live in updates.ts. */
export interface Article {
	num: string;
	title: string;
	date: string;
	route: string;
}

export const articles: Article[] = [
	{ num: '02', title: 'Private disks, shared bytes', date: '2026-09-14', route: 'updates/2' },
	{ num: '03', title: 'Reads no longer wait on writers', date: '2026-09-15', route: 'updates/3' },
	{ num: '04', title: 'The whole system, one diagram', date: '2026-09-16', route: 'updates/4' },
	{ num: '05', title: 'How CAS changed', date: '2026-09-24', route: 'updates/5' }
];

export const articleSource = (route: string) =>
	`https://github.com/harivansh-afk/cas/blob/main/playbook/src/routes/${route}/+page.svelte`;
