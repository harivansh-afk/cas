export type NativeArm = 'raw' | 'daemon' | 'cas';
export interface NativeValue {
	n: number;
	median: number;
	min: number;
	max: number;
}
export interface NativeMetric {
	id: string;
	label: string;
	unit: string;
	values: Partial<Record<NativeArm, NativeValue>>;
}
export interface NativeReport {
	date: string;
	source_revision: string;
	status: string;
	baseline: NativeMetric[];
	pressure: NativeMetric[];
	accounting: NativeMetric[];
	completed: number;
	failed: number;
	failed_runs: { backend: string; name: string; error: string }[];
	notes: string[];
}
