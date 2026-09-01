export interface Page {
	num: string;
	title: string;
	description: string;
}

export const pages: Page[] = [
	{ num: '00', title: 'Thesis', description: 'The claim · hypotheses H1–H3 · hardware class · assumptions A1–A8' },
	{ num: '01', title: 'The model', description: 'An LSM tree whose compaction step is content addressing · the comparison ladder' },
	{ num: '02', title: 'Distribution', description: 'Local write, global dedup · what distributes for free · what stays hard' },
	{ num: '03', title: 'Measurement', description: 'Census, comparison, demonstration · gates G1–G4 · schedule' }
];
