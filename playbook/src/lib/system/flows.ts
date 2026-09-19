import type { Flow } from './graph';

/** One request or one background job traced through the parts it touches, in order. */
export const flows: Flow[] = [
	{
		id: 'write',
		name: 'WRITE',
		result: 'The block is readable once its batch is published in order. Nothing is hashed yet, and nothing is durable until a FLUSH.',
		steps: [
			{ node: 'guest-blk', text: 'The guest posts a descriptor chain (16-byte header, data, status byte) and kicks the queue.' },
			{ node: 'frontier', via: ['vendor-backend', 'frontier'], text: 'The epoll worker wakes the frontend. The chain is snapshotted, parsed and recorded DISCOVERED in the carrier before any admission decision.' },
			{ node: 'fe-admission', via: ['frontier', 'fe-admission'], text: 'A fairness ticket and turn are obtained, then storage reserves a request credit and a WAL window slot. A refusal leaves the payload in guest RAM and retries in 100 ms.' },
			{ node: 'local', via: ['fe-admission', 'local'], text: 'The payload is gathered from guest memory straight into the open batch Builder and the mutation sequence is assigned. The carrier slot moves PREPARED then ACTIVE.' },
			{ node: 'reactor', via: ['local', 'reactor'], text: 'At the end of the pass the batch is sealed and sent through the bounded mailbox; the reactor prepares the append at the next segment offset.' },
			{ node: 'io-uring', via: ['reactor', 'io-uring'], text: 'When the IO scheduler grants a demand turn, one positional O_DIRECT write of header plus payload is submitted.' },
			{ node: 'wal-submit', via: ['reactor', 'wal-submit'], text: 'On completion the reactor publishes appends in order under the image gate: index mappings are replaced, P advances, the carrier records it.' },
			{ node: 'frontend', via: ['reactor', 'local'], text: 'The completion returns through the response mailbox; the frontend writes status OK, adds the used entry and signals the call eventfd. Dropping the permit releases the slot, the entry and the fairness turn.' }
		]
	},
	{
		id: 'flush',
		name: 'FLUSH',
		result: 'Every mutation admitted before the FLUSH is durable in the WAL. A failed sync never advances E and never acknowledges the FLUSH.',
		steps: [
			{ node: 'fe-admission', via: ['frontier', 'fe-admission'], text: 'FLUSH is a control request: it uses the control reserve instead of a fairness ticket, so a FLUSH on another queue can pass while bulk admission waits.' },
			{ node: 'local', via: ['fe-admission', 'local'], text: 'The open batch is sealed and the flush is tagged with the current admitted mutation boundary.' },
			{ node: 'reactor', via: ['local', 'reactor'], text: 'If the boundary is already durable the flush completes at once. If a fence cohort is open and covers it, the flush joins it. Otherwise a FENCE record is prepared for the issued prefix.' },
			{ node: 'io-uring', via: ['reactor', 'io-uring'], text: 'The 4 KiB fence header is written immediately, bypassing the bulk arbiter; later writes stay unsubmitted so the cohort is finite.' },
			{ node: 'wal-submit', via: ['reactor', 'wal-submit'], text: 'Once every covered append has published, an fdatasync is submitted. Its completion moves durable (E) to the boundary.' },
			{ node: 'frontend', via: ['reactor', 'local'], text: 'All waiters of the cohort complete; the guest filesystem finishes its fsync.' }
		]
	},
	{
		id: 'read',
		name: 'READ',
		result: 'Recent staged writes and ZERO ranges override the committed manifest. Only uncovered blocks reach the shared cache and chunk store, and one fetch per hash serves every waiting reader.',
		steps: [
			{ node: 'frontier', via: ['vendor-backend', 'frontier'], text: 'A read behind a write that is waiting for capacity is eligible if their ranges do not overlap and no FLUSH sits between them; it is admitted as the queue\'s read candidate.' },
			{ node: 'local', via: ['fe-admission', 'local'], text: 'A read request credit and bytes + 1 MiB of read bytes are reserved; the read is tagged with the admitted boundary.' },
			{ node: 'read', via: ['local', 'reactor'], text: 'The reactor starts it once that boundary has published, builds a ReadPlan that pins the staged ranges and clones the committed View.' },
			{ node: 'wal-read', via: ['read', 'wal-read'], text: 'Staged ranges are read from the WAL and CRC-verified against the whole original write.' },
			{ node: 'man-file', via: ['read', 'man-file'], text: 'Each uncovered block descends the B+tree one page at a time, served from the page cache or one direct page read.' },
			{ node: 'cache', via: ['read', 'cache'], text: 'A hash is looked up in the clean chunk cache; a hit is copied into the response.' },
			{ node: 'fills', via: ['read', 'fills'], text: 'On a miss the reader becomes the leader for that hash or a waiter polling the leader\'s eventfd.' },
			{ node: 'store-read', via: ['read', 'store-read'], text: 'The leader resolves the address, reads the batch header and the 4 KiB payload, verifies CRC and BLAKE3, and offers a copy to the cache.' },
			{ node: 'frontend', via: ['reactor', 'local'], text: 'The owned response is copied into the guest descriptors, then status and used entry are published.' }
		]
	},
	{
		id: 'compact',
		name: 'COMPACT',
		result: 'Chunks are durable before the manifest names them; the manifest is published before its WAL space is reclaimed. A newer guest write stays in staging and keeps precedence.',
		steps: [
			{ node: 'port', text: 'After 100 ms without writes, 1 s of dirty age or staging pressure, the reactor queues a compaction turn for its image.' },
			{ node: 'owner', via: ['port', 'owner'], text: 'The compactor thread takes the turn and asks the reactor for a Selection.' },
			{ node: 'wal-compaction', via: ['port', 'wal-compaction'], text: 'Under the sequencer: capture the base View, the cursor and scan-pinned spans through the durable boundary E.' },
			{ node: 'compact', via: ['compact', 'wal-compaction'], text: 'Load whole batches above D up to 1 MiB and 318 edits, verify payload, drop versions later edits cover, hash surviving nonzero blocks, build the COW pages in memory.' },
			{ node: 'store-insert', via: ['compact', 'store-insert'], text: 'Reserve index growth and a physical promise, write missing chunks in batches of 63, fdatasync, publish their addresses.' },
			{ node: 'man-file', via: ['compact', 'man-file'], text: 'Append the new pages and a COMMIT naming the root and D, fdatasync, swap the current commit.' },
			{ node: 'port', via: ['port', 'owner'], text: 'The reactor installs the new View, advances D, drops staging mappings at or below D and selects reclaimable spans.' },
			{ node: 'wal-reclaim', via: ['port', 'wal-reclaim'], text: 'Payload of covered batches nobody pins is hole-punched; fully covered old segments are unlinked; the staging quota reopens and waiting writers are woken.' }
		]
	},
	{
		id: 'collect',
		name: 'COLLECT',
		result: 'Every image pauses while the host reclaims chunk segments and manifest pages. If live data still fills usable space, write admission stays closed and the pressure is reported.',
		steps: [
			{ node: 'owner', text: 'An administrative request, or the physical governor pressured for a second, claims the single control slot.' },
			{ node: 'quiesce', via: ['collect', 'quiesce'], text: 'Admission pauses across all images, including reads; frontends and reactors are woken to see it.' },
			{ node: 'reactor', via: ['port', 'owner'], text: 'Each reactor drains pending IO, fences to cover its published prefix and acknowledges the quiescence generation.' },
			{ node: 'store-collection', via: ['collect', 'store-collection'], text: 'Marking walks every manifest root, pinned view and snapshot. Mixed segments have their live chunks copied into a fresh ticket under a background promise; dead ones are unlinked.' },
			{ node: 'man-reclaim', via: ['collect', 'man-reclaim'], text: 'Every manifest and snapshot punches pages no retained root reaches.' },
			{ node: 'compact', via: ['port', 'owner'], text: 'While the disk is still pressured, one quiescent compaction alternates with another sweep until nothing advances.' },
			{ node: 'quiesce', via: ['collect', 'quiesce'], text: 'The governor observes real allocation; admission resumes if the reserve is restored, otherwise capacity is reported exhausted.' }
		]
	},
	{
		id: 'reconnect',
		name: 'RECONNECT',
		result: 'The same guests keep running through a daemon replacement. It needs the original QEMU processes, guest RAM and inflight memfds; without them the host recovers cold from the catalog.',
		steps: [
			{ node: 'hostsvc', text: 'A replacement cas-host --mode retained locks and inspects every catalog image read-only, without repairing anything.' },
			{ node: 'qemu-carrier', via: ['qemu-carrier', 'carrier'], text: 'QEMU reconnects each socket and hands back the retained memfd in SET_INFLIGHT_FD; the frontend validates its header, identity and slots.' },
			{ node: 'fe-recovery', via: ['fe-recovery', 'carrier'], text: 'Reconciliation reads each queue\'s used index, adopts interrupted completions, restores DISCOVERED heads to admission and re-decodes every retained head from the descriptor table.' },
			{ node: 'host-recovery', via: ['fe-recovery', 'host-recovery'], text: 'The coordinator waits for every image\'s validated bundle, then requires each WAL\'s valid prefix plus D to cover the saved P.' },
			{ node: 'wal-recovery', via: ['host-recovery', 'wal-recovery'], text: 'Missing mutations are rebuilt one batch each from surviving guest RAM with their original identities, then a recovery FENCE is written and synced per image.' },
			{ node: 'frontend', via: ['fe-recovery', 'carrier'], text: 'Behind one barrier every image activates: WRITE, ZERO and FLUSH completions are restored, READs re-enter the reactor, and ordinary admission opens.' }
		]
	},
	{
		id: 'experiment',
		name: 'EXPERIMENT',
		result: 'A result exists only as a typed report bound to a source revision and executable hashes, kept under results/ and summarized in docs/. Every number on the site is imported from one of those files.',
		steps: [
			{ node: 'nix', text: 'nix build .#async-smoke (or a fixture, the checkpoint bundle, casctl) produces a wrapper with a build record naming the guest launcher, daemon and source.' },
			{ node: 'h-vm', via: ['nix', 'workspace'], text: 'The wrapper execs cas-harness, which refuses to run unless it is the binary in that record.' },
			{ node: 'host', via: ['h-vm', 'host'], text: 'The harness allocates a scratch image, starts cas-daemon or cas-host on a fresh socket and waits for it.' },
			{ node: 'qemu', via: ['h-vm', 'qemu'], text: 'The NixOS guest boots with the vhost-user-blk device; the oneshot runs fio against the disk and writes results over a 9p share.' },
			{ node: 'harness', via: ['h-suite', 'h-vm'], text: 'Reports are decoded with hard conditions: bytes written and read, flushes, zero errors, complete drain, copy identities and credit bounds.' },
			{ node: 'evidence', via: ['harness', 'evidence'], text: 'The session is appended to docs/validation.md, the measurement gets a README and JSON, TODO.md links the record, and the playbook imports the JSON.' }
		]
	}
];
