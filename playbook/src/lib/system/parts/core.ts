import type { SystemNode } from '../graph';

/** cas-core: formats, indexes, accounting and scheduling primitives (crate cas-core). */
export const core: SystemNode[] = [
	{
		id: 'core',
		title: 'cas-core',
		sub: 'library · formats, indexes, accounting',
		tone: 'accent',
		what: 'The storage library. It owns every byte format and its recovery parser, the in-memory indexes, the copy-on-write manifest tree, the caches, the budget system that charges before allocating, disk accounting and the bulk-IO arbiter. It has no threads of its own; the daemon drives it.',
		how: [
			'Two crate-wide constants: BLOCK_SIZE = 4096 and MAX_REQUEST_BYTES = 1 MiB. Virtio sectors are a separate 512-byte unit.',
			'The invariants are carried by types: aligned buffers, leases that release on drop, permits that must be finished, receipts only successful IO can produce, formats validated on decode and re-decoded after encode.',
			'The review found no data-loss, ordering or lock-order bug in the storage, replay, carrier or cache paths; every unsafe block has a justification.'
		],
		files: ['crates/cas/core/src/lib.rs', 'Cargo.toml'],
		docs: ['rust-design', 'storage-design', 'storage-format']
	},

	/* ---------- WAL ---------- */
	{
		id: 'append',
		parent: 'core',
		title: 'WAL (append log)',
		sub: 'Log · v2 packed batches',
		tone: 'accent',
		what: 'The per-image write-ahead log. It encodes packed batches, tracks the three sequence counters issued ≥ published ≥ durable, publishes mutations into an interval index in order, writes fences for FLUSH, and offers selection, publication and reclamation interfaces to compaction.',
		how: ['A WRITE completes at published (P); a FLUSH waits for durable (E); compaction later moves the manifest\'s D forward. During serving D ≤ E ≤ P.', 'Nothing is ever overwritten in place; a segment is preallocated, appended, punched and finally unlinked.'],
		types: ['Log', 'Config', 'Limits', 'Status'],
		files: ['crates/cas/core/src/append.rs'],
		docs: ['storage-design', 'wal-allocation']
	},
	{
		id: 'wal-format',
		parent: 'append',
		title: 'Batch codec',
		sub: 'Builder · Header · SegmentHeader',
		what: 'The v2 byte layout and its validator. A batch is one 4 KiB header (64-byte envelope plus up to 63 descriptors of 64 bytes) followed by packed whole-block payload up to 1 MiB. A FENCE is a header alone. Decoding checks magic, version, CRC, counts, dense sequences, payload coverage and range arithmetic before any field is trusted.',
		how: ['Builder gathers guest bytes into their final position, records each payload CRC, then seals the header without moving payload.', 'follows() is the chain rule recovery, compaction and reclamation share: the next batch must fit before the segment\'s fence slot and continue the sequence.'],
		numbers: [['descriptors per batch', '63'], ['max batch', '4 KiB + 1 MiB'], ['magic', 'CASSEG02 · CASBAT02']],
		files: ['crates/cas/core/src/append/format.rs'],
		docs: ['storage-format']
	},
	{
		id: 'wal-index',
		parent: 'append',
		title: 'Staging index',
		sub: 'interval map · preallocated nodes',
		what: 'A disjoint interval map from logical byte ranges to the newest published payload location or ZERO. It is what makes recent writes take precedence over the manifest on reads.',
		how: ['A pinned B-tree port over a preallocated node pool: 65,536 intervals reserve 13,174 slots of 1 KiB, about 12.9 MiB per image, charged to metadata up front so publication can never fail on memory.', 'Replacing a range can split one straddling predecessor and remove covered entries, so each descriptor costs at most two intervals.', 'Compaction drops every mapping at or below D.'],
		files: ['crates/cas/core/src/append/index.rs', 'crates/cas/core/src/append/index/nodes.rs'],
		docs: ['staging-metadata']
	},
	{
		id: 'wal-submit',
		parent: 'append',
		title: 'Submission + cohorts',
		sub: 'issued · published · durable',
		tone: 'outline',
		what: 'The counters and the rules around them. prepare_append seals a batch at the next offset and advances issued; publish_append refuses unless the batch is the next in order; prepare_fence captures the boundary of a finite cohort; complete_sync advances durable only after every covered append published.',
		how: ['A batch that completes out of order waits as Pending until its predecessor publishes.', 'A 4 KiB fence slot is reserved at the end of every segment so a FLUSH can always be recorded without rolling over.', 'A failed sync never advances E; a poisoned log refuses everything after.'],
		types: ['Submission', 'Position', 'Cohort'],
		files: ['crates/cas/core/src/append/submission.rs'],
		docs: ['persistence-model']
	},
	{
		id: 'wal-segment',
		parent: 'append',
		title: 'Segments + pins',
		sub: 'preallocated files · read pins · rotation',
		what: 'Segment files and their ownership. Creation encodes the header, preallocates the capacity, syncs the file and its directory before any data. Pins are one atomic per 4 KiB block so a read can hold the batch it depends on and a compaction scan can hold a whole segment.',
		how: ['Rotation is three-phase so file creation can run on the owner thread: prepare under the sequencer, create off it, install and recheck the boundary.', 'A fresh attachment\'s writer epoch is its own segment ticket.'],
		files: ['crates/cas/core/src/append/segment.rs', 'crates/cas/core/src/append/segment/pins.rs', 'crates/cas/core/src/append/rotation.rs'],
		docs: ['segment-allocation']
	},
	{
		id: 'wal-read',
		parent: 'append',
		title: 'Read plans',
		sub: 'ReadPlan · covered mask',
		what: 'Builds the immutable plan a read executes: the staged ranges it covers (with pins acquired), a 256-bit mask of covered blocks, and a clone of the committed manifest View for the rest. Dropping the plan releases the pins.',
		how: ['A plan is Pending until the log has published the boundary the read captured.', 'Executing a range reads directly into the response when the whole payload is wanted, or into scratch with a full-payload CRC check otherwise.'],
		files: ['crates/cas/core/src/append/read.rs'],
		docs: ['read-ownership']
	},
	{
		id: 'wal-recovery',
		parent: 'append',
		title: 'WAL recovery',
		sub: 'inspect · fresh · live replay',
		tone: 'outline',
		what: 'Reopens a log. Inspection replays every batch header in ticket order without trusting anything after the first failure, skips punched payload below D through intact headers, verifies payload above D, and records where the valid prefix ends. Repair archives the rejected suffix, truncates, syncs, and writes a recovery fence.',
		how: ['Cold: rotate to a new epoch and fence before serving.', 'Live: accept up to 1,024 retained identities, require the missing tail to be exactly the retained mutations above the prefix, re-verify every retained mutation at or below the prefix byte for byte against its descriptor, then replay one batch per mutation in original order.', 'A prefix below D is an error, never a shorter replay.'],
		types: ['Recovery', 'LivePlan', 'LiveRecovery', 'SharedRecovery'],
		files: ['crates/cas/core/src/append/recovery.rs', 'crates/cas/core/src/append/shared.rs'],
		docs: ['shared-recovery', 'staging-base']
	},
	{
		id: 'wal-compaction',
		parent: 'append',
		title: 'Selection + output',
		sub: 'Selection · Input · Prepared',
		what: 'The core of one compaction transaction. Selection captures the base View, a cursor and scan-pinned spans. Loading scans headers from the cursor, verifies payload above D and collects at most 318 edits and 1 MiB. Preparing drops covered edits, hashes survivors and builds the COW pages. Writing inserts chunks, then publishes the manifest, returning a receipt naming the exact new root.',
		files: ['crates/cas/core/src/append/compaction.rs', 'crates/cas/core/src/append/compaction/output.rs'],
		docs: ['compaction', 'compaction-crash-cuts']
	},
	{
		id: 'wal-reclaim',
		parent: 'append',
		title: 'Reclamation',
		sub: 'punch payload · unlink segments',
		what: 'Frees WAL space below D. A DATA batch entirely at or below D whose header block no reader pins and whose segment no scan holds has its payload hole-punched; a segment is unlinked only when fully covered, not current, below the ticket high-water mark and below the oldest live replay identity, with no other owners.',
		how: ['Punched batches keep their headers so recovery can chain past them.', 'Physical release is measured with st_blocks, never assumed.'],
		files: ['crates/cas/core/src/append/reclaim.rs'],
		docs: ['manifest-reclamation', 'wal-allocation']
	},

	/* ---------- chunk store ---------- */
	{
		id: 'store',
		parent: 'core',
		title: 'Chunk store',
		sub: 'Store · Reader · shared content',
		tone: 'accent',
		what: 'The shared, immutable content store under chunks/. One writer (the compactor) inserts batches of verified 4 KiB chunks; any number of readers resolve a hash to its segment and block without holding the writer through IO. An index entry is usable only after its batch is synced.',
		types: ['Store', 'Reader', 'Shared', 'Segment'],
		files: ['crates/cas/core/src/store.rs', 'crates/cas/core/src/store/file.rs'],
		docs: ['chunk-store-io', 'store-readers']
	},
	{
		id: 'chunk',
		parent: 'store',
		title: 'Chunking + hashing',
		sub: 'fixed 4 KiB · BLAKE3 · zero = hole',
		what: 'Content identity. A chunk is one 4 KiB block named by its BLAKE3-256 hash; an all-zero block is not stored at all and becomes a manifest hole. Boundaries are fixed logical blocks; content-defined chunking remains an unimplemented census arm.',
		how: ['A one-byte edit makes a new chunk; identical blocks share even across unrelated images.', 'The index compares full hashes and does not byte-compare a duplicate against disk.'],
		files: ['crates/cas/core/src/chunk.rs'],
		docs: ['census']
	},
	{
		id: 'chunk-index',
		parent: 'store',
		title: 'Hash index',
		sub: 'hash → 48-bit ticket · 16-bit block',
		tone: 'outline',
		what: 'The in-memory map from 32-byte hash to a 64-bit store address, in a hashbrown table on the budgeted allocator with a GC mark bit per entry. Growth is reserved from the metadata budget before the chunks it would describe are written; denial leaves existing lookups intact.',
		how: ['Rebuilt on recovery from the inline hashes in every valid chunk record, so CRC checks suffice and BLAKE3 is not recomputed.', 'Every stored hash needs RAM, including dead entries awaiting collection: unique capacity is bounded by memory as well as disk.'],
		numbers: [['address', 'segment ≤ 2⁴⁸−1, block ≤ 65,535 (256 MiB segment)']],
		files: ['crates/cas/core/src/chunk_index.rs'],
		docs: ['cache-table-capacity', 'index-admission']
	},
	{
		id: 'store-format',
		parent: 'store',
		title: 'Chunk batch codec',
		sub: 'CASCHS02 · CASCHB02 · 63 chunks',
		what: 'The chunk segment header and batch envelope mirror the WAL layout with their own magic. A 64-byte descriptor per chunk carries the hash, payload offset, length (exactly 4096) and CRC32; ordinals are dense per segment. No fence is needed: the data sync gates index publication.',
		files: ['crates/cas/core/src/store/format.rs'],
		docs: ['storage-format']
	},
	{
		id: 'store-insert',
		parent: 'store',
		title: 'Insert',
		sub: 'dedupe · reserve · write · sync · publish',
		what: 'The writer path. Dedupe against the index and within the batch, reserve index growth, pick or create a segment, snapshot the offset under the lock, then without the lock preallocate, write and fdatasync, and only then publish each hash. Any exit with the output pending poisons the store.',
		files: ['crates/cas/core/src/store/file/insert.rs'],
		docs: ['chunk-store-io']
	},
	{
		id: 'store-read',
		parent: 'store',
		title: 'Reader',
		sub: 'plan → header → payload · verified',
		what: 'A cloneable handle that resolves a hash under a short mutex to a file, batch and address, then advances through header and payload IO on the caller\'s ring or thread. The header descriptor must repeat the hash; the payload must match its CRC.',
		how: ['Lookups return WouldBlock during collection so a reader never sees a segment mid-move.'],
		files: ['crates/cas/core/src/store/file/read.rs'],
		docs: ['store-readers']
	},
	{
		id: 'store-collection',
		parent: 'store',
		title: 'Collection + sweep',
		sub: 'mark bits · copying cleaner',
		what: 'Garbage collection of chunk segments. Marking sets the bit on every reachable index entry; victims are classified dead, live or mixed. A mixed victim\'s live chunks are re-read, verified, rehashed and appended into a fresh ticket, then the old segment is trimmed and unlinked. Unmarked entries are removed at the end.',
		how: ['Requires no outstanding read plan on any segment file, which is why the host quiesces first.', 'A crash before unlink leaves duplicate chunks; a crash after keeps the durable copy. Either way one copy of every reachable chunk survives.'],
		files: ['crates/cas/core/src/store/file/collection.rs', 'crates/cas/core/src/store/file/collection/sweep.rs'],
		docs: ['chunk-collection']
	},
	{
		id: 'store-recovery',
		parent: 'store',
		title: 'Store inspection',
		sub: 'rebuild index · archive tails',
		what: 'Opens chunks/ read-only, requires canonical names, walks every batch header for continuity and CRC, stops at the first invalid one, and rebuilds the hash index from the inline hashes. Recovery archives each tail and rejected creation, truncates, syncs and unlinks.',
		files: ['crates/cas/core/src/store/file/recovery.rs'],
		docs: ['chunk-store-io']
	},

	/* ---------- manifest ---------- */
	{
		id: 'manifest',
		parent: 'core',
		title: 'Manifest',
		sub: 'COW B+tree · block → hash',
		tone: 'accent',
		what: 'The per-image map from logical block to content hash, as a persistent copy-on-write B+tree of checksummed 4 KiB pages in an append-only file. A COMMIT page ends every transaction; readers pin one committed root and never see a half-written tree.',
		how: ['It is a page-addressed tree, not a Merkle tree: pages are found by file offset, hashes live only in leaves.', 'Height is at most 8; a change touches at most 8H + 8 new pages, which bounds a transaction to under 90 MiB of new pages.'],
		files: ['crates/cas/core/src/manifest.rs'],
		docs: ['manifest-editor', 'manifest-views']
	},
	{
		id: 'man-format',
		parent: 'manifest',
		title: 'Page codec',
		sub: 'CASMAN02 · LEAF · BRANCH · COMMIT',
		what: 'The 64-byte page header (magic, kind, level, count, own offset, CRC) and the three bodies: 63 leaf extents of 64 bytes, 252 branch children of 16 bytes, or a COMMIT naming store, image, generation, root, height, D and image bytes. Every encoder re-decodes its output as a self-check.',
		files: ['crates/cas/core/src/manifest/format.rs'],
		docs: ['storage-format']
	},
	{
		id: 'man-tree',
		parent: 'manifest',
		title: 'Tree + COW editor',
		sub: 'Lookup · Prepared · 8H+8 bound',
		tone: 'outline',
		what: 'Traversal and mutation. Lookup is an IO-neutral state machine that asks for one page at a time and can be driven by a synchronous read or an io_uring completion. The editor applies up to 318 edits in memory, detaches subtrees a ZERO covers without reading them, splits only over-full nodes, and emits only pages reachable from the final root.',
		how: ['Every decoded node is checked against its parent\'s level, minimum key and bounds, so a duplicate child under the wrong key is rejected beyond the CRC.', 'A draft edit updates one path at a time; grouping edits by path is still open work.'],
		files: ['crates/cas/core/src/manifest/tree.rs', 'crates/cas/core/src/manifest/tree/lookup.rs', 'crates/cas/core/src/manifest/tree/editor.rs', 'crates/cas/core/src/manifest/tree/node.rs'],
		docs: ['manifest-editor', 'manifest-views']
	},
	{
		id: 'man-file',
		parent: 'manifest',
		title: 'Manifest file',
		sub: 'Manifest · View · Read',
		what: 'The locked owner of manifest.v2 and the immutable views over it. publish requires the prepared transaction to start at the current commit and end, reserves the successor pin, preallocates, writes, fdatasyncs, then swaps the commit; any failure leaves the owner poisoned with the old root still published.',
		how: ['A View clones the file handle, the commit and a root pin; a Read in flight keeps both alive after the View and even the Manifest are dropped.', 'inspect scans pages backwards from EOF and selects the newest COMMIT whose whole tree walks; a torn transaction leaves the earlier root.'],
		files: ['crates/cas/core/src/manifest/file.rs'],
		docs: ['manifest-views', 'manifest-root-pins']
	},
	{
		id: 'man-pins',
		parent: 'manifest',
		title: 'Root registry',
		sub: 'pins · incarnation',
		what: 'A per-file registry of retained roots keyed by commit end. Every View, in-flight Read and snapshot holds a pin; page reclamation captures all of them to decide what is reachable. Each registry has a process-unique incarnation that is part of the page-cache key, so a reopened file cannot inherit stale pages.',
		files: ['crates/cas/core/src/manifest/file/pins.rs'],
		docs: ['manifest-root-pins']
	},
	{
		id: 'man-cache',
		parent: 'manifest',
		title: 'Page cache',
		sub: '16 MiB · verified pages only',
		what: 'A host-wide LRU of verified 4 KiB tree pages keyed by (incarnation, commit end, offset). Only a page that passed the checked descent may enter; a page a reader still holds cannot be evicted, and a refusal is counted rather than failed.',
		how: ['Because the key includes the commit end, every new publication misses on unchanged interior pages: an open design item.', 'It lives inside the 128 MiB foreground metadata budget, not beside it.'],
		files: ['crates/cas/core/src/manifest/file/cache.rs'],
		docs: ['metadata-cache']
	},
	{
		id: 'man-reclaim',
		parent: 'manifest',
		title: 'Page reclamation',
		sub: 'mark reachable · punch gaps',
		what: 'Frees dead manifest pages in place. It requires the exact owned EOF, re-verifies every retained COMMIT, walks each pinned root once collecting live offsets under the metadata budget, sorts and dedupes them, punches every gap in at most 128 MiB slices, and trims preallocation beyond EOF.',
		how: ['The file\'s logical end never shrinks: history is punched, not compacted away.'],
		files: ['crates/cas/core/src/manifest/file/reclaim.rs'],
		docs: ['manifest-reclamation']
	},
	{
		id: 'man-snapshot',
		parent: 'manifest',
		title: 'Snapshots + clones',
		sub: 'FICLONE at an exact COMMIT',
		what: 'A snapshot is a reflink of a pinned View truncated to its commit end, synced with its directory. A clone reflinks a snapshot and appends one COMMIT for the new image with generation 1 and D = 0; the shared pages diverge by copy-on-write from then on.',
		files: ['crates/cas/core/src/manifest/file/snapshot.rs'],
		docs: ['snapshot-files']
	},

	/* ---------- cache ---------- */
	{
		id: 'cache',
		parent: 'core',
		title: 'Clean chunk cache',
		sub: '256 MiB LRU · read fills only',
		tone: 'accent',
		what: 'A host-wide cache of verified chunk payload keyed by full hash. Only verified read fills populate it; writes and compaction never prewarm it. Eviction drops membership while a reader keeps its own clone and charge.',
		how: ['Shared by every image with no partition and no scan resistance: one guest\'s scan can displace another\'s hot set.', 'A fill recomputes BLAKE3 before insertion and makes a separate charged copy so the foreground read credit is not held forever.'],
		files: ['crates/cas/core/src/cache.rs'],
		docs: ['clean-cache']
	},
	{
		id: 'lru',
		parent: 'cache',
		title: 'Intrusive LRU',
		sub: 'hashbrown table · fixed capacity',
		what: 'A hash table whose entries link to their older and newer neighbours, so promotion and eviction are lookups rather than scans. It reserves twice the resident capacity up front; the pinned hashbrown version rehashes tombstones in place, so churn never grows the table.',
		files: ['crates/cas/core/src/cache/lru.rs'],
		docs: ['cache-table-capacity']
	},
	{
		id: 'fills',
		parent: 'cache',
		title: 'Coalesced fills',
		sub: '1 leader · bounded waiters · eventfd',
		tone: 'outline',
		what: 'One fetch per missing hash across the host. The first reader becomes the leader and fetches; concurrent readers become waiters holding a nonblocking eventfd the leader writes on completion or failure. A leader that drops without finishing publishes Interrupted.',
		numbers: [['bounds', '1,024 leaders · 1,024 waiters']],
		files: ['crates/cas/core/src/cache/fills.rs', 'crates/cas/core/src/eventfd.rs'],
		docs: ['coalesced-fills']
	},

	/* ---------- budget ---------- */
	{
		id: 'budget',
		parent: 'core',
		title: 'Budgets',
		sub: 'charge before allocate · release on drop',
		tone: 'accent',
		what: 'Two-dimensional counters (bytes and requests) with a hard limit, and four ways to hold a charge: a raw Lease, a Share nesting an image cap inside a host cap, an Allocator that charges every layout before the global allocator sees it, and a BudgetArc whose control block is charged too.',
		how: ['The daemon creates one 128 MiB foreground metadata budget and one 128 MiB compaction budget and threads the same handles into every core object; there is no unbudgeted constructor on the Linux paths.', 'Growth of a table charges old and new storage for a moment, which is why index reservation happens before store IO.'],
		files: ['crates/cas/core/src/budget.rs', 'crates/cas/core/src/budget/allocator.rs', 'crates/cas/core/src/budget/shared.rs'],
		docs: ['shared-allocation', 'daemon-owner-allocation']
	},
	{
		id: 'channel',
		parent: 'budget',
		title: 'Bounded mailboxes',
		sub: 'Queue · channel · never grows',
		what: 'A fixed ring of slots reserved at construction and a mutex-plus-condvar channel over it. try_send never blocks; close rejects new publication while draining what is queued; a poisoned lock fails closed and wakes everyone. The daemon uses these for every reactor, owner and reply channel.',
		files: ['crates/cas/core/src/budget/queue.rs', 'crates/cas/core/src/budget/channel.rs'],
		docs: ['control-mailbox']
	},

	/* ---------- scheduler, space, misc ---------- */
	{
		id: 'scheduler',
		parent: 'core',
		title: 'IO scheduler',
		sub: 'bulk opportunities · 3 demand : 1 background',
		tone: 'outline',
		what: 'Arbitrates who may hand the next bulk IO to the kernel: reactors with queued appends and reads, or the compactor thread\'s direct reads and writes. Every fourth opportunity is reserved for background; unused turns are borrowed by the other side.',
		how: ['A reactor marks itself ready and takes a turn only when it is the current selection; the scheduler wakes it through its bound eventfd.', 'The compactor waits at most 30 s per syscall; expiry fails that transaction, which the review flagged as a parameter to revisit.', 'FLUSH fences and eventfd polls bypass the arbiter entirely.'],
		files: ['crates/cas/core/src/scheduler.rs'],
		docs: ['host-scheduling', 'congestion-wait']
	},
	{
		id: 'space',
		parent: 'core',
		title: 'Disk accounting',
		sub: 'Governor · Staging · reserve R',
		tone: 'outline',
		what: 'Physical and logical capacity. The Governor observes real allocation with fstatvfs and admits foreground promises below capacity − R and one exclusive background borrower within R. Staging is the per-image and host WAL quota with the 75 % / cap / 60 % hysteresis. Every promise is finished against a measured allocation or fails the account.',
		how: ['Recovery paths borrow a governed handle so repairs and archives are accounted like any other output.', 'The arithmetic-only Space is used by production only through the Governor.'],
		numbers: [['reserve', 'R = 3S + M + 16 MiB'], ['margin', '16 MiB']],
		files: ['crates/cas/core/src/space.rs', 'crates/cas/core/src/space/filesystem.rs', 'crates/cas/core/src/space/staging.rs', 'crates/cas/core/src/space/recovery.rs'],
		docs: ['physical-space', 'staging-capacity']
	},
	{
		id: 'catalog',
		parent: 'core',
		title: 'Catalog',
		sub: 'membership · atomic publish',
		what: 'The durable list of images and snapshots. Changes are prepared in memory with the next generation, written to a pending file, synced, renamed over catalog.v2 and the directory synced. Inspection reads only the live file and never adopts a pending one.',
		files: ['crates/cas/core/src/catalog.rs', 'crates/cas/core/src/catalog/format.rs'],
		docs: ['catalog']
	},
	{
		id: 'tickets',
		parent: 'core',
		title: 'Segment tickets',
		sub: 'one namespace · highest header wins',
		what: 'The store-wide allocator for segment numbers. Opening scans chunks/, every image\'s staging/ and their rejected/ archives for the highest name; allocation is highest + 1 and only succeeds once the new header is durable. A failed creation poisons every allocator user.',
		files: ['crates/cas/core/src/segments.rs'],
		docs: ['segment-allocation']
	},
	{
		id: 'direct',
		parent: 'core',
		title: 'IO primitives',
		sub: 'direct · aligned · directory · encoding',
		what: 'The shared low level. AlignedBuffer is a 4 KiB-aligned boxed slice that cannot be misaligned by construction. direct opens with O_DIRECT and flock, checks alignment via statx, rejects short IO, and wraps fallocate, punch, reflink, FIEMAP and sync. Directory locks, syncs, renames and archives rejected suffixes. encoding is the little-endian and CRC32 helper set.',
		how: ['Test-only fault injection can fail or pause any of these syscalls once, which is how the persistence oracle explores crash schedules.'],
		files: ['crates/cas/core/src/aligned.rs', 'crates/cas/core/src/direct.rs', 'crates/cas/core/src/directory.rs', 'crates/cas/core/src/encoding.rs'],
		docs: ['async-io']
	},
	{
		id: 'staging-v1',
		parent: 'core',
		title: 'v1 staging log',
		sub: 'legacy · 8 KiB slots · serial',
		tone: 'muted',
		what: 'The original single-file log from Update 01: fixed 8 KiB slots of one metadata block and one payload block, CRC per block, a FENCE that can only be a metadata block. Still used by casctl staging-check and the daemon\'s --backend staging control. No conversion path to v2 exists.',
		files: ['crates/cas/core/src/staging.rs', 'crates/cas/core/src/staging/format.rs'],
		docs: ['history/spec-v1']
	},
	{
		id: 'census',
		parent: 'core',
		title: 'Census',
		sub: 'offline · 4 KiB and 16 KiB',
		tone: 'muted',
		what: 'The offline measurement behind Update 01\'s sharing numbers. It hashes raw images at both chunk sizes with the same nonzero-BLAKE3 rule as the compactor and partitions each image\'s bytes into base, prior-image, within-image and unique classes. It uses ordinary collections and never runs in the daemon.',
		files: ['crates/cas/core/src/census.rs'],
		docs: ['census']
	},
	{
		id: 'io-metrics',
		parent: 'core',
		title: 'IO metrics',
		sub: 'thread-local scope counters',
		tone: 'muted',
		what: 'Fifteen operation counters (reads, writes, syncs, allocations, punches, hashing, lock waits, scheduler waits) accumulated inside a thread-local scope. The compactor wraps each turn in one; the counts feed the per-phase compaction telemetry.',
		files: ['crates/cas/core/src/io_metrics.rs'],
		docs: ['pressure-telemetry']
	}
];
