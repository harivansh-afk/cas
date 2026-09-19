import census from './census.json';
import spark from './clone-updates.json';

/**
 * Meeting updates. A slide is a few plain lines and maybe one diagram, the
 * way notes are written on paper. Numbers come from the evidence JSON.
 */

export type Figure = 'built' | 'stack' | 'log' | 'ring' | 'chunk' | 'updates' | 'copies' | 'tracks';

export interface Slide {
	id: string;
	/**
	 * What is written on the slide, top to bottom. The first line is the thought.
	 * A line starting with "- " is a point under the line above it.
	 * `[text](url)` renders as a link.
	 */
	lines: string[];
	figure?: Figure;
}

export interface Update {
	slug: string;
	number: string;
	title: string;
	date: string;
	minutes: number;
	slides: Slide[];
}

export const source = 'https://github.com/harivansh-afk/cas/blob/main/playbook/src/lib/updates.ts';

const snapshot = 'https://git.harivan.sh/harivansh-afk/cas-research/src/commit/0c2e958888d15cfb3d07df4650c9c08031eaa764';
const site = 'https://harivansh-afk.github.io/cas';
const man = (page: string, section: number) => `https://man7.org/linux/man-pages/man${section}/${page}.${section}.html`;
const virtio = 'https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html';
const vhostUser = 'https://www.qemu.org/docs/master/interop/vhost-user.html';
const backendCrate = 'https://github.com/rust-vmm/vhost/tree/main/vhost-user-backend';
const gb = (bytes: number) => `${(bytes / 1e9).toFixed(2)} GB`;
const pct = (ratio: number) => `${(100 * ratio).toFixed(0)}%`;

/* Two dated Ubuntu root images, deduplicated alone and together. */
export const pair = census.census.results.map((r) => ({
	label: `${r.chunk_bytes / 1024} KiB`,
	alone: r.independently_unique_bytes,
	together: r.fleet_unique_bytes,
	saved: r.cross_image_duplicate_bytes / r.independently_unique_bytes
}));

/* Three clones of one image, per epoch and chunk size; ancestor held out. */
export const fleet = spark.epochs[0].census.results.map(({ chunk_bytes }) => ({
	label: `${chunk_bytes / 1024} KiB`,
	periods: spark.epochs.map((epoch) => {
		const { descendants } = epoch.census.results.find((r) => r.chunk_bytes === chunk_bytes)!;
		return {
			label: epoch.label,
			unique: descendants.fleet_unique_bytes,
			base: descendants.base_content_duplicate_bytes,
			novel: descendants.novel_content_duplicate_bytes
		};
	})
}));

const p4 = pair[0];
const p16 = pair[1];
const t2 = fleet[0].periods.at(-1)!;
const t2w = fleet[1].periods.at(-1)!;
const t2total = t2.unique + t2.base + t2.novel;

export const updates: Update[] = [
	{
		slug: '1',
		number: '01',
		title: 'Guest <> Host reads/writes',
		date: '2026-09-10',
		minutes: 25,
		slides: [
			{
				id: 'contract',
				lines: [
					'The write path: a log, a sequence number, and a fence.',
					'The guest sees a normal [virtio-blk](' + virtio + '#x1-2850003) disk. Behind it is the daemon, my process, which QEMU talks to over a [vhost-user](' + vhostUser + ') socket.',
					'Write',
					'- Appended to one file, the staging log, stamped with a sequence number. Nothing is overwritten in place.',
					'FLUSH',
					'- The guest saying: everything before this must survive a power cut.',
					'- The daemon appends a fence record and calls [fdatasync](' + man('fdatasync', 2) + '). The same contract a physical disk gives.',
					'Not on this path',
					'- Hashing. It happens later, in the compactor, which is not built yet.'
				],
				figure: 'built'
			},
			{
				id: 'stack',
				lines: [
					'Where the daemon sits: guest driver, QEMU, vhost-user-backend, my code.',
					'The guest kernel\'s virtio-blk driver puts requests into two rings in guest memory.',
					'QEMU sets the device up, hands the daemon the ring addresses and a file descriptor for guest memory over a unix socket, then leaves the data path.',
					'[vhost-user-backend](' + backendCrate + '), from rust-vmm, speaks that socket protocol: it maps guest memory into the daemon, runs the event loop, and calls my code when the guest kicks a queue.',
					'My code reads the request out of guest memory, runs it against the storage worker, writes the status byte, and publishes the used entry.'
				],
				figure: 'stack'
			},
			{
				id: 'direct',
				lines: [
					'[O_DIRECT](' + man('open', 2) + '): writes go to the NVMe, not to host RAM.',
					'Normally a write lands in the host kernel\'s page cache and returns at once; the kernel writes it to the NVMe later. If the host dies in between, the only copy was in RAM. O_DIRECT bypasses that cache: a write returns once the NVMe has accepted it. [fdatasync](' + man('fdatasync', 2) + ') still flushes the device\'s own cache, so FLUSH is unchanged.',
					'Tradeoffs',
					'- Every buffer must be 4 KiB aligned in memory or the kernel rejects it. A Rust type with a compile-time check makes a misaligned buffer impossible to construct.',
					'- Every 4 KiB guest write is one 8 KiB device write today, synchronously. The fix is batching in the daemon, not in the kernel: the daemon knows where the FLUSH boundaries are. Measured before G1.',
					'- The host page cache was a free read cache, and it is gone. Metadata-heavy work, many small reads, renames, stats, is where a second-level cache pays.',
					'The read cache is not decided',
					'- A chunk cache inside the daemon, keyed by hash, size a parameter.',
					'- Buffered reads on the host, so the page cache is a second level under the guest\'s own.',
					'- virtio-pmem with DAX for the immutable base image: the one place a shared mapping is a read cache.'
				]
			},
			{
				id: 'recover',
				lines: [
					'Recovery: finding where the log ends.',
					'A fence is the record FLUSH appends. It names the last sequence number that is now durable. Recovery trusts everything up to the last fence and nothing after it.',
					'Problem',
					'- After a crash the tail is garbage: a half-written record, or zeros from preallocation. And the guest\'s own data can contain bytes that look exactly like a fence.',
					'Format',
					'- Fixed 8 KiB slots, a 4 KiB metadata block and a 4 KiB data block, each with a [CRC32](https://en.wikipedia.org/wiki/Cyclic_redundancy_check) checksum. A fence can only be a metadata block, so guest bytes are never mistaken for one.',
					'Recovery',
					'- Scan back to the last fence with a valid checksum, replay forward checking every record, cut everything after. A bad record before that fence is an error, never a shorter replay.'
				],
				figure: 'log'
			},
			{
				id: 'inflight',
				lines: [
					'Live recovery: the daemon dies mid-request.',
					'Requests live in two rings in memory shared between guest and daemon: the guest posts to one, the daemon posts completions to the other. A replacement daemon sees the rings, but not which requests the old one had started.',
					'vhost-user has a mechanism for this, [inflight I/O tracking](' + vhostUser + '#inflight-i-o-tracking): QEMU hands the backend a shared file to record per-request state in, and a replacement reads it back. vhost-user-backend 0.23 returns unsupported for both of those messages.',
					'Workaround today: one request in flight, made durable before it is marked used, so the used counter alone is the resume point. Killed at four points inside a request; the same guest keeps running. Too slow for any latency number; lifting it is the next two weeks.'
				],
				figure: 'ring'
			},
			{
				id: 'device',
				lines: [
					'Implementing virtio-blk by hand.',
					'Addressing',
					'- The guest is told blocks are 4 KiB, but a [virtio-blk request](' + virtio + '#x1-2850003) counts in 512-byte sectors, a rule from the original spec. Sector 8 means byte 4096.',
					'- Anything that is not whole 4 KiB blocks is rejected.',
					'Framing',
					'- A request arrives as a chain of memory descriptors; the 16-byte header can be split across them or share one with the data.',
					'- Parsing works on bytes, not descriptors. Tested at all 17 split positions.',
					'Negotiation',
					'- A guest that will not negotiate FLUSH is refused before any IO, instead of acknowledging unsynced writes as stable.',
					'Ordering',
					'- [io_uring](' + man('io_uring_enter', 2) + ') does not order an fsync behind earlier writes unless asked; the drain flag does that.',
					'- An empty discard once took a sequence number nothing wrote, so the next FLUSH waited forever. Now it takes none.'
				]
			},
			{
				id: 'chunk',
				lines: [
					'Chunk size: the decision the compactor gets built around.',
					'The compactor is not built. It will take settled data out of the log, cut it into chunks, hash each with [BLAKE3](https://github.com/BLAKE3-team/BLAKE3), and store every distinct chunk once. Chunk size is the one choice everything downstream depends on.',
					'Against 4 KiB',
					'- Memory. Every distinct chunk needs an index entry, a 32-byte hash and an 8-byte offset: about 10 GB of RAM per TB at 4 KiB, a quarter of that at 16 KiB.',
					'For 4 KiB',
					'- Change one 4 KiB block inside a 16 KiB chunk and the whole chunk is new. The other 12 KiB of sharing is lost every time.',
					'Third option, untested',
					'- [Content-defined chunking](https://www.usenix.org/conference/atc16/technical-sessions/presentation/xia): cut where the bytes say to, using a rolling hash, so an insert moves one boundary instead of every chunk after it. Cuts are rounded to 4 KiB so chunks still line up with the guest\'s blocks.'
				],
				figure: 'chunk'
			},
			{
				id: 'census',
				lines: [
					'The census: measuring before committing.',
					`Two dated Ubuntu images share ${pct(p4.saved)} of their bytes at 4 KiB, ${pct(p16.saved)} at 16 KiB.`,
					`Three clones of one image, each upgraded on its own. T0 is the control: everything shared, nothing new. By T2, ${gb(t2.novel)} of new duplicates that only hashing can see.`,
					`At T2 the three guests need ${gb(t2.unique)} at 4 KiB and ${gb(t2w.unique)} at 16 KiB: 16 KiB costs ${pct(t2w.unique / t2.unique - 1)} more for the same guests.`,
					'Proves 4 KiB gets the index. Proves nothing about speed.'
				],
				figure: 'updates'
			},
			{
				id: 'pmem',
				lines: [
					'[virtio-pmem](https://www.qemu.org/docs/master/system/devices/virtio-pmem.html) for reads: examined, parked.',
					'What it does',
					'- Maps one host file into every guest as memory. With [DAX](https://docs.kernel.org/filesystems/dax.html) the guest reads the host\'s pages directly, no copy in its own cache: one resident copy of a base image per host.',
					'What it costs',
					'- Fixed at boot, immutable while mapped. The host never sees the writes, so it cannot hash. Needs a flat file per image per host: a second copy of the bytes dedup collapses.',
					'Where it still fits',
					'- As the read cache for the immutable base image, one of the three candidates on the O_DIRECT slide. It shares what is already known to be equal; the amber on the census slide is what it cannot see.'
				],
				figure: 'copies'
			},
			{
				id: 'next',
				lines: ['Next', 'Write path first: no number is reported before recovery and ordering pass.'],
				figure: 'tracks'
			}
		]
	}
];
