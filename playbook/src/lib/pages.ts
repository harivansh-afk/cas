export interface Page {
	num: string;
	title: string;
	description: string;
}

export const pages: Page[] = [
	{ num: '00', title: 'Thesis', description: 'Goal · prior art and the unmeasured split · outcome · hypotheses · hardware · assumptions' },
	{ num: '01', title: 'The model', description: 'An LSM tree whose compaction step is content addressing · rungs R0–R3' },
	{ num: '02', title: 'Distribution', description: 'Local write, global dedup · what stays hard · the transport slot' },
	{ num: '03', title: 'Measurement', description: 'Census, comparison, demonstration · gates G1–G5 · schedule' },
	{ num: '04', title: 'Implementation', description: 'What is stock and what is new · repository layout · build order' }
];
