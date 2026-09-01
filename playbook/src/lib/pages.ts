export interface Page {
	num: string;
	title: string;
	description: string;
}

export const pages: Page[] = [
	{ num: '00', title: 'Overview', description: 'Claims, functional bar, scope, novelty' },
	{ num: '01', title: 'CAS store', description: 'Chunk log, index, block map, integrity, liveness' },
	{ num: '02', title: 'VMM integration', description: 'virtio-blk contract, batching, comparison arms' },
	{ num: '03', title: 'Hot paths', description: 'Write, read, FLUSH, DISCARD · stages T0–T7 · diagram' },
	{ num: '04', title: 'Measurement system', description: 'Instrumentation, arms, benchmarks, validation gates' },
	{ num: '05', title: 'Repo and plan', description: 'Tree, milestones, stretch goals, risks' }
];
