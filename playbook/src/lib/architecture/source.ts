/** Current implementation links; the consolidated C5 run has its own older revision. */
export const revision = '4b50554';
const commit = '4b505548f97513d65fa25f193cca67dd83aa8ee5';
export const testedRevision = 'b8399ad';
export const source = (path: string) =>
	`https://git.harivan.sh/harivansh-afk/cas-research/src/commit/${commit}/${path}`;
// Reports were committed after the runtime they measure.
export const record = (path: string) =>
	`https://git.harivan.sh/harivansh-afk/cas-research/src/commit/e0460e8114a13898b39fc47c05d354f86f334ad0/${path}`;
export const doc = (name: string) => record(`docs/${name}.md`);

/** Links pinned to an arbitrary revision; Update 03 uses the merged review integration. */
export const sourceAt = (commit: string) => (path: string) =>
	`https://git.harivan.sh/harivansh-afk/cas-research/src/commit/${commit}/${path}`;
