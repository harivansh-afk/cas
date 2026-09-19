import type { SystemNode } from '../graph';

/** cas-host: the daemon process (crate cas-daemon). */
export const host: SystemNode[] = [
	{
		id: 'host',
		title: 'cas-host',
		sub: 'one process · crate cas-daemon',
		tone: 'accent',
		what: 'The daemon that serves every image of one store. Per image it runs a vhost-user frontend thread and a reactor thread; per host it runs one compactor thread, a supervisor and a set of shared resources. It owns the guest protocol, ordering, admission and recovery; the formats and indexes come from cas-core.',
		how: [
			'cas-host init creates a store; cas-host --image <id>=<socket>... serves all catalog images, cold or retained.',
			'The older cas-daemon binary serves one image with a chosen backend: raw io_uring, the v1 staging log, or the same local runtime; the suite keeps those as controls.',
			'Failure is one shared domain: a socket worker error cancels every image. Image gates can fail one image, but the executable escalates.',
			'Coordination uses threads, budgeted bounded channels, mutexes and eventfds. No cgroups, no async runtime.'
		],
		numbers: [['threads', '1 socket + 1 epoll per image, 1 reactor per image, 1 cas-compactor, supervisor'], ['deadlines', '30 s per submitted IO and drain, 60 s recovery']],
		files: ['crates/cas/daemon/src/bin/cas-host.rs', 'crates/cas/daemon/src/main.rs', 'crates/cas/daemon/src/lib.rs'],
		docs: ['host-runtime', 'host-service', 'rust-design']
	},

	/* ---------- per-image frontend ---------- */
	{
		id: 'frontend',
		parent: 'host',
		title: 'Image frontend',
		sub: 'Backend · one per image',
		tone: 'accent',
		what: 'The vhost-user backend for one image: it maps guest memory, parses requests out of the rings, decides admission, keeps an owned record of every admitted request, and publishes completions. It runs on the framework\'s epoll worker thread under one mutex.',
		how: [
			'Every event: drain finished IO first, then visit each vring starting from a rotating queue, discovering and admitting requests.',
			'Admission happens before any payload is copied; a waiting write stays in guest RAM with no mutation number.',
			'Completion writes read payload, then the status byte, then add_used and the call eventfd, all through the accepted memory snapshot.',
			'The struct is 30 fields spread across admission, frontier, lifecycle and recovery files that share its private state, which the September review called overgrown.'
		],
		types: ['Backend', 'VhostUserBackendMut', 'PendingRequest', 'Report'],
		files: ['crates/cas/daemon/src/backend.rs'],
		docs: ['shared-frontend', 'frontend-allocation']
	},
	{
		id: 'vendor-backend',
		parent: 'frontend',
		title: 'vhost-user-backend',
		sub: 'rust-vmm 0.23.0 · vendored + patched',
		what: 'The pinned rust-vmm framework that speaks the socket protocol: it maps SET_MEM_TABLE regions, creates the vrings, registers kick eventfds in an epoll loop and calls the backend trait. Two narrow patches were added and pinned.',
		how: [
			'Patch 1: begin/end state-change hooks around every frontend message so the backend can quiesce before memory or queue changes.',
			'Patch 2: GET/SET_INFLIGHT_FD forwarded to the backend instead of rejected, which is the whole live-recovery channel.',
			'virtio-queue 0.18.0 is patched only to make DescriptorChain::new public so replay can walk a saved head whose ring slot has wrapped.',
			'One socket thread decodes messages; one epoll worker owns all vrings of the image.'
		],
		files: ['crates/vendor/README.md', 'crates/vendor/vhost-user-backend-0.23.0/src/handler.rs', 'crates/vendor/virtio-queue-0.18.0/CAS-PATCH.md'],
		docs: ['vhost-user-notes']
	},
	{
		id: 'request',
		parent: 'frontend',
		title: 'Request parser',
		sub: 'bytes, not descriptors',
		what: 'Decodes one descriptor chain into a typed Request: Read, Write, Zero, Flush, GetId, Unsupported or Invalid. It works on the byte stream, so a header split across descriptors or sharing one with data parses the same way.',
		how: [
			'Takes the first 16 readable bytes as the header and the last writable byte as the status, whatever the descriptor boundaries; tested at all 17 split positions.',
			'Converts sectors to bytes and rejects anything not whole aligned 4 KiB blocks within capacity or above 1 MiB.',
			'DISCARD and WRITE_ZEROES decode one 16-byte range; both become a ZERO mutation of at most 1 MiB. Unknown types answer UNSUPP, not an error.'
		],
		types: ['request::parse', 'Request', 'Segment', 'Completion'],
		files: ['crates/cas/daemon/src/request.rs'],
		docs: ['zero-discard']
	},
	{
		id: 'frontier',
		parent: 'frontend',
		title: 'Discovery + read bypass',
		sub: 'Frontier · descriptor snapshots',
		tone: 'outline',
		what: 'Separates seeing a request from admitting it. Every descriptor on a queue is snapshotted and recorded DISCOVERED in the carrier as soon as it appears, so an independent read behind a write that is waiting for capacity can be admitted first.',
		how: [
			'A later request is eligible only if it is a read and overlaps no older discovered write, zero or discard; FLUSH conflicts with everything.',
			'One ordinary head and one read candidate per queue are tried each pass, each through its own admission slot, so fairness tickets stay bounded.',
			'Snapshot memory for the worst case, four queues of 256 chains, is reserved from the metadata budget up front: 6,297,600 bytes per frontend.',
			'Discovered-but-unadmitted requests return to admission after a daemon replacement; they are never replayed as writes.'
		],
		types: ['Frontier', 'Discovered', 'eligible', 'conflicts'],
		files: ['crates/cas/daemon/src/backend/frontier.rs'],
		docs: ['read-progress']
	},
	{
		id: 'fe-admission',
		parent: 'frontend',
		title: 'Queue admission',
		sub: 'one waiting head per queue · 100 ms retry',
		what: 'Holds at most one waiting request per queue with the reason it waits: fairness turn, pending table full, or a storage pressure reason. A refused head keeps its descriptor in the guest ring and is retried on release wakeups and a 100 ms timer.',
		how: [
			'Asks the shared fair scheduler for a ticket and a turn, then asks storage to reserve credits; success commits the turn into the permit so the final credit drop wakes the next image.',
			'Waits never expire into IOERR: the docs\' five-second admission deadline was removed and the code says so explicitly.',
			'Statistics per queue (started, resumed, canceled, longest wait) are what Update 03\'s admission-wait numbers come from.'
		],
		types: ['QueueAdmission', 'Waiting', 'Reason'],
		files: ['crates/cas/daemon/src/backend/admission.rs'],
		docs: ['congestion-wait', 'admission-release-wakeup']
	},
	{
		id: 'pending',
		parent: 'frontend',
		title: 'Pending table',
		sub: 'owned request records',
		what: 'A fixed-capacity table of every admitted request, inserted before its payload is gathered or handed to storage and removed only when the completion returns ownership. Nothing admitted can disappear, and nothing can complete twice.',
		how: ['Capacity is the image request limit, 144 on the shared host, charged once at construction.', 'On failure every record whose queue is still valid gets one IOERR; the records stay for the terminal drain.'],
		types: ['Pending<PendingRequest>'],
		files: ['crates/cas/daemon/src/backend/pending.rs'],
		docs: ['lifecycle', 'control-tables']
	},
	{
		id: 'lifecycle',
		parent: 'frontend',
		title: 'State-change brackets',
		sub: 'quiesce · drain · rebase · resume',
		what: 'Wraps every QEMU configuration message in a pause. Before memory tables, queue geometry, enablement or reset change, the frontend stops admission, drains owned IO against the old state, and asks the reactor for a barrier; afterwards it validates cursors and resumes.',
		how: [
			'A drain has the 30 s IO deadline; timing out fails the image but never releases kernel-owned buffers or locks.',
			'Queues touched by a change are marked blocked and rebase; a queue is served again only once its guest used index matches ours.',
			'Device reset clears features and blocks every queue; a fresh GET_INFLIGHT_FD after serving starts a new writer epoch and carrier.'
		],
		types: ['begin_change', 'end_change', 'rebase_queue', 'validate_used_cursors'],
		files: ['crates/cas/daemon/src/backend/lifecycle.rs'],
		docs: ['lifecycle', 'queue-setup-diagnostics']
	},
	{
		id: 'carrier',
		parent: 'frontend',
		title: 'Inflight carrier',
		sub: 'the memfd\'s state machine',
		tone: 'outline',
		what: 'The daemon side of the retained memfd: creates and seals it, records discovery and admission per head with acquire/release atomics, publishes the prefix P after ordered publication, and clears a slot only after the guest\'s used entry is written.',
		how: [
			'A PREPARED slot reserves its sequence even if the process dies before the counters update; reconciliation adopts it.',
			'complete requires the request\'s publication boundary to be at or below P unless the image has failed.',
			'Replacement: attach validates the header and every slot, reconcile compares each queue\'s used index (equal or one ahead), collects DISCOVERED slots in order and returns the replay set.'
		],
		types: ['Carrier', 'Entry', 'Replay', 'reconcile'],
		files: ['crates/cas/daemon/src/inflight.rs', 'crates/cas/daemon/src/inflight/layout.rs', 'crates/cas/daemon/src/inflight/recovery.rs', 'crates/cas/daemon/src/inflight/mapping.rs'],
		docs: ['inflight-format']
	},
	{
		id: 'fe-recovery',
		parent: 'frontend',
		title: 'Attachment session',
		sub: 'AwaitingFd → Replay → Waiting → Active',
		what: 'Negotiates the carrier with QEMU and drives a replacement daemon back to serving. Fresh attachments get a new epoch and carrier; a retained fd is reconciled, its missing mutations replayed from the original guest buffers, and its completions restored before admission opens.',
		how: [
			'Retained heads are re-decoded from the descriptor table with the patched DescriptorChain::new, then checked against the carrier\'s identity.',
			'Standalone: replay runs on a cas-recovery thread under the 60 s deadline. Shared host: the frontend submits a Validated bundle and waits for the host coordinator to activate every image together.',
			'On activation WRITE, ZERO and FLUSH completions are published at once (they are durable and fenced); READs re-enter the reactor; rejected entries get IOERR.'
		],
		types: ['Session', 'Phase', 'activate_attachment', 'finish_attachment', 'Validated'],
		files: ['crates/cas/daemon/src/backend/recovery.rs'],
		docs: ['shared-live-recovery', 'storage-design']
	},
	{
		id: 'service',
		parent: 'frontend',
		title: 'Connection + report',
		sub: 'Service · Control',
		what: 'Owns one socket listener through connect, serve, disconnect and drain, and writes the final JSON report. The Control handle lets the supervisor snapshot the backend for telemetry or cancel it.',
		how: ['Registers the completion eventfd and timerfd on the epoll loop before taking the backend lock, because registration calls back into the backend.', 'A disconnect drains accepted IO without touching guest memory and records pending counts before and after.'],
		types: ['Service', 'Control', 'FinalReport'],
		files: ['crates/cas/daemon/src/service.rs'],
		docs: ['disconnect-drain-evidence', 'main-wrapper-integration']
	},
	{
		id: 'observability',
		parent: 'frontend',
		title: 'Faults + read tracing',
		sub: 'SIGSTOP points · CAS_TRACE_READS',
		tone: 'muted',
		what: 'Test-only hooks. Fault points stop the process at named write boundaries so the crash harness can cut at PREPARED, ACTIVE, before submit, after the append CQE, before and after sync, and around replay. Read tracing attributes each read\'s latency to admission, dispatch, IO and completion.',
		how: ['A pause publishes a JSON marker then raises SIGSTOP; the harness kills and replaces the daemon at that cut.', 'Tracing keeps 64-bucket histograms and the slowest examples; it is what located the 7.9 s admission wait behind an unrelated write.'],
		files: ['crates/cas/daemon/src/fault.rs', 'crates/cas/daemon/src/read_trace.rs', 'crates/cas/daemon/src/local/host/fault.rs'],
		docs: ['read-tracing', 'shared-crash-controls']
	},

	/* ---------- per-image adapter and reactor ---------- */
	{
		id: 'local',
		parent: 'host',
		title: 'Local adapter',
		sub: 'Local · runs on the frontend thread',
		what: 'The per-image storage adapter between the frontend and the reactor. It reserves credits, packs writes straight into their final WAL batch buffer, assigns mutation numbers, and hands commands to the reactor through a bounded mailbox.',
		how: [
			'Admit: enter the host quiescence gate, take a request credit, reserve WAL window space (which checks staging and physical capacity), then an append credit and a batch if needed. Any refusal is a typed pressure reason.',
			'Gather: copy guest payload into the open Builder at its final position and record its CRC; the mutation sequence advances here.',
			'Seal: a batch closes at 63 descriptors, 1 MiB, a non-write, a refusal, or the end of a processing pass. No timer holds a lone write.',
			'A Permit travels with each request and releases, in order, its WAL slot, its admission entry and its fairness turn.'
		],
		types: ['Local', 'Shared', 'Permit', 'Packing', 'Command'],
		files: ['crates/cas/daemon/src/local.rs', 'crates/cas/daemon/src/local/pressure.rs'],
		docs: ['wal-admission', 'index-admission', 'control-mailbox']
	},
	{
		id: 'window',
		parent: 'local',
		title: 'WAL window',
		sub: 'reserve before mutation',
		what: 'A per-image reservation window over the current WAL segment. It guarantees that every admitted write, plus its framing and a fence, fits in the segment and the staging interval index before a mutation number is assigned.',
		how: ['used = end + reserved + (issued − fenced + unsubmitted + initial fence) × 4 KiB; a candidate that would overflow marks rotation wanted and wakes the reactor.', 'Index pressure (unsubmitted mappings at capacity) refuses with WalIndex and asks for a compaction fence.', 'A dropped unused slot refunds itself and wakes the reactor.'],
		types: ['Window', 'Slot'],
		files: ['crates/cas/daemon/src/local/window.rs'],
		docs: ['wal-admission', 'wal-allocation']
	},
	{
		id: 'pools',
		parent: 'local',
		title: 'Credit pools',
		sub: 'host budgets · image shares',
		what: 'Request and byte budgets that bound what a guest can have in flight. Each image holds a Share of the host pools; a reservation takes the host lease first and rolls it back if the image cap refuses.',
		numbers: [['write requests', '128 per image · 1,024 per host'], ['read requests', '8 per image · 64 per host'], ['append bytes', '8 MiB per image · 64 MiB per host'], ['read bytes', '8 MiB per image · 64 MiB per host'], ['control reserve', '8 / 64 KiB per image · 32 / 256 KiB per host'], ['read reservation', 'bytes + 1 MiB scratch']],
		types: ['HostPools', 'Pools', 'Share', 'Credits'],
		files: ['crates/cas/daemon/src/local/pools.rs'],
		docs: ['daemon-owner-allocation']
	},
	{
		id: 'reactor',
		parent: 'host',
		title: 'Image reactor',
		sub: 'thread local_async · owns the WAL + io_uring',
		tone: 'accent',
		what: 'One thread per image that executes storage IO in one write order. It appends batches, publishes completions contiguously, runs fence cohorts and fdatasync, resolves reads through staging, manifest and chunk store, and sequences background transactions with the compactor.',
		how: [
			'Each iteration: receive commands, reap completions, enforce deadlines, poll the background port, publish appends in order, refresh the window, advance ready work, dispatch new commands, submit bulk IO under the scheduler, then wait on two eventfds.',
			'A later append never publishes before an earlier one completes; a slow write holds back later completions, in exchange for one visibility order.',
			'A FLUSH captures a boundary and starts a fence cohort; later writes wait outside it so the sync stays finite.',
			'While a rotation is pending, mutation dispatch stops but reads and covered flushes continue.'
		],
		types: ['Reactor', 'Work', 'Pending', 'Slots', 'dispatch', 'finish_ready'],
		files: ['crates/cas/daemon/src/local/reactor.rs', 'crates/cas/daemon/src/local/reactor/slots.rs'],
		docs: ['async-io', 'control-tables', 'physical-runtime']
	},
	{
		id: 'read',
		parent: 'reactor',
		title: 'Read state machine',
		sub: 'staging → manifest → cache → chunk',
		tone: 'outline',
		what: 'A read waits for its captured mutation boundary to publish, freezes the staged ranges and manifest root it will use, then walks the uncovered blocks one at a time: manifest page lookups through the page cache, then the shared chunk cache, then a coalesced fetch from the chunk store.',
		how: [
			'Staged WAL ranges win, including explicit ZERO; a partial read verifies the whole original payload CRC before copying a sub-range.',
			'A cache miss claims a fetch leader for that hash; other readers register a waiter and poll the leader\'s eventfd instead of issuing their own IO.',
			'The fetched block is verified by CRC and BLAKE3, offered to the cache as a separate charged copy, then copied into the response.',
			'One 1 MiB cold read does not submit its 256 fetches at once; that sequential walk is an open cost.'
		],
		types: ['Read', 'Stage', 'next_block', 'chunk'],
		files: ['crates/cas/daemon/src/local/reactor/read.rs', 'crates/cas/core/src/append/read.rs'],
		docs: ['read-ownership', 'coalesced-fills', 'metadata-cache']
	},
	{
		id: 'port',
		parent: 'reactor',
		title: 'Background port',
		sub: 'Port::poll · turns for the compactor',
		what: 'The reactor side of every background transaction. It decides when the image is ready for a compaction or rotation turn, hands the compactor a selection under the sequencer, installs receipts, and applies reclamation accounting.',
		how: [
			'A compaction turn is queued when durable data is uncompacted and either 100 ms have passed without a write, the oldest dirty data is 1 s old, or staging, index or host pressure forces it.',
			'Events from the owner: Select, Published, Reclaimed, Allocate, Rotated, Deferred, Quiesce, Resume, CompactQuiescent; each answered by a typed Reply.',
			'A background transaction that gets no reply for 30 s fails the image; the owner keeps whatever files and buffers it holds.'
		],
		types: ['Port', 'Event', 'Reply', 'Turn', 'Ready'],
		files: ['crates/cas/daemon/src/local/host.rs'],
		docs: ['host-runtime', 'compaction']
	},

	/* ---------- shared host owner ---------- */
	{
		id: 'owner',
		parent: 'host',
		title: 'Compactor owner',
		sub: 'thread cas-compactor · one per host',
		tone: 'accent',
		what: 'The single background thread. It holds the chunk store writer and every image\'s mutable manifest, so all compaction, WAL rotation, collection and snapshot IO is serialized on it, with blocking file IO rather than io_uring.',
		how: [
			'Loop: if the disk is pressured and a second passed, collect; else take one Ready item within 50 ms: an image turn (compact or rotate), a collect request or a snapshot request.',
			'Every direct read or write on this thread first asks the IO scheduler for a background opportunity.',
			'Errors fail the host gate when the store or an account failed, otherwise only that image\'s gate; a panic fails the host.'
		],
		types: ['Owner', 'Endpoint', 'Ready'],
		files: ['crates/cas/daemon/src/local/host/worker.rs'],
		docs: ['host-runtime', 'host-scheduling']
	},
	{
		id: 'compact',
		parent: 'owner',
		title: 'Compaction turn',
		sub: 'select → load → prepare → write → publish → reclaim',
		tone: 'outline',
		what: 'Turns a bounded, durable WAL prefix into shared chunks and a new manifest root. Data is durable before the mapping that names it; the mapping is published before the WAL space it covers is reclaimed.',
		how: [
			'Select (reactor, under the sequencer): whole batches above D up to E, at most 1 MiB payload and 318 edits, resumed from a validated cursor.',
			'Load and prepare (owner): read and CRC-verify the payload, drop versions covered by later edits, hash surviving nonzero blocks with BLAKE3, build the copy-on-write pages in memory.',
			'Write: reserve a physical promise, insert missing chunks in batches of 63 and sync, append manifest pages and COMMIT and sync.',
			'Publish (reactor): adopt the new View, advance D, drop staging mappings at or below D; then the owner punches or unlinks WAL payload not pinned by readers or replay identities.'
		],
		numbers: [['settle · forced age', '100 ms · 1 s'], ['per transaction', '≤ 1 MiB input, ≤ 318 edits, ≤ 128 MiB new pages']],
		files: ['crates/cas/daemon/src/local/host/worker.rs', 'crates/cas/core/src/append/compaction.rs', 'crates/cas/core/src/append/compaction/output.rs'],
		docs: ['compaction', 'compaction-crash-cuts']
	},
	{
		id: 'rotate',
		parent: 'owner',
		title: 'WAL rotation',
		sub: 'new segment · fresh attachment',
		what: 'Allocates the next staging segment for an image when its window would overflow or a fresh attachment needs a new writer epoch. The reactor prepares under its sequencer; the owner creates, preallocates and syncs the file; the reactor installs it.',
		how: ['The staging quota is reserved before the file exists; the promise is finished against the measured allocation.', 'A rollover fence syncs the old segment before the new one accepts data.'],
		files: ['crates/cas/daemon/src/local/host/worker.rs', 'crates/cas/core/src/append/rotation.rs'],
		docs: ['wal-allocation', 'segment-allocation']
	},
	{
		id: 'collect',
		parent: 'owner',
		title: 'Host collection',
		sub: 'pause all · mark · copy · unlink · punch',
		tone: 'outline',
		what: 'Quiescent mark-and-sweep across every image. It pauses all admission including reads, drains accepted work, fences every image, marks chunks reachable from every manifest root, snapshot and pinned view, copies live chunks out of mixed segments, unlinks dead ones, and hole-punches dead manifest pages.',
		how: [
			'Triggered by an administrative request or automatically once a second while the physical governor is pressured.',
			'Under pressure it alternates one quiescent compaction with another sweep until nothing advances, then reports capacity exhausted with write admission still closed.',
			'The 30 s check sits between steps, not inside blocking syscalls or tree walks, so a long overwrite history can pause guests longer.'
		],
		files: ['crates/cas/daemon/src/local/host/collection.rs', 'crates/cas/core/src/store/file/collection.rs', 'crates/cas/core/src/store/file/collection/sweep.rs'],
		docs: ['host-collection', 'chunk-collection', 'host-quiescence']
	},
	{
		id: 'snapshot',
		parent: 'owner',
		title: 'Snapshot',
		sub: 'compact to a cut · reflink · catalog',
		what: 'Publishes an exact snapshot of one image under the same host pause: compact through the fenced cut, reflink the manifest at that COMMIT, insert the catalog entry, sync. It needs a catalog owner and a physical governor, so only recovered hosts can snapshot.',
		how: ['The cut is the image gate\'s durable sequence; compaction repeats until the manifest\'s D reaches it.', 'A clone gets its own image identity, WAL and sequence namespace but starts from the snapshot\'s mapping.'],
		files: ['crates/cas/daemon/src/local/host/snapshots.rs', 'crates/cas/core/src/manifest/file/snapshot.rs'],
		docs: ['host-snapshots', 'snapshot-files']
	},
	{
		id: 'capacity',
		parent: 'owner',
		title: 'Capacity control',
		sub: 'staging 75 % / cap / 60 % · reserve R',
		what: 'The rules that connect disk accounting to admission. Staging pressure starts compaction at 75 % of an image or host quota, stops write admission at the quota and resumes below 60 %. Physical pressure preserves the background reserve and forces collection.',
		how: ['At startup every current segment, and their sum, must sit strictly below 60 % of the staging quota; at defaults that permits at most nine images.', 'A compaction reserves one segment plus chunk headers plus manifest bytes plus a 16 MiB margin before it writes.'],
		numbers: [['reserve R', '3S + M + 16 MiB = 336 MiB at defaults'], ['staging quota', '256 MiB per image · 1 GiB per host']],
		files: ['crates/cas/daemon/src/local/host/capacity.rs'],
		docs: ['live-capacity-control', 'staging-capacity', 'physical-collection-readiness']
	},

	/* ---------- shared runtime state ---------- */
	{
		id: 'shared',
		parent: 'host',
		title: 'Shared host state',
		sub: 'SharedHost · one per process',
		what: 'What every image shares: the chunk store reader, the two caches, the fetch registry, the admission scheduler, the quiescence gate, the failure gates and the accounts. Built once from one metadata budget before any image attaches.',
		types: ['SharedHost', 'Resources', 'Roots'],
		files: ['crates/cas/daemon/src/local/host.rs'],
		docs: ['host-runtime', 'daemon-owner-allocation']
	},
	{
		id: 'fair',
		parent: 'shared',
		title: 'Fair admission',
		sub: 'FIFO per image · byte deficit round robin',
		tone: 'outline',
		what: 'Decides which guest request head may be admitted next across images. Eight heads per image (an ordinary head and a read candidate per queue), FIFO among an image\'s eligible heads, and a 1 MiB byte quantum rotated across images.',
		how: [
			'A ticket registers a head; a turn is granted only if that exact head is chosen, otherwise the chosen image\'s frontend is woken.',
			'A refused turn now rotates its head behind the image\'s other heads and advances the cursor. Before the 14 September fix a refused write was re-chosen every retry and starved the read behind it for 8.1 s.',
			'A release that races a refused turn still marks heads ready, closing the missed-wakeup bug found on 13 September.'
		],
		types: ['Fair', 'Ticket', 'Turn', 'Release'],
		files: ['crates/cas/daemon/src/local/host/fair.rs'],
		docs: ['host-scheduling', 'admission-release-wakeup']
	},
	{
		id: 'quiesce',
		parent: 'shared',
		title: 'Quiescence gate',
		sub: 'Admission · counts live owners',
		what: 'Counts guest owners in flight and can stop new ones atomically. Collection and snapshots pause it, wait until the count is zero, run, and resume; dropping a started pause without finishing fails the gate closed.',
		types: ['Admission', 'Entry', 'Quiescence'],
		files: ['crates/cas/daemon/src/local/host/admission.rs'],
		docs: ['host-quiescence']
	},
	{
		id: 'gates',
		parent: 'shared',
		title: 'Completion gates',
		sub: 'HostGate → ImageGate',
		what: 'The failure words. Locking an image gate locks the host gate first and copies any shared failure in, so a completion can never be published past a poisoned store. The frontend holds the guard from its storage decision through the guest\'s used entry.',
		how: ['A store output failure poisons the host gate before the worker is told.', 'A WAL or manifest failure stays scoped to its image while the shared store is healthy.'],
		types: ['HostGate', 'Gate', 'ImageState'],
		files: ['crates/cas/daemon/src/local/state.rs'],
		docs: ['host-runtime', 'recovery-lock-readiness']
	},

	/* ---------- host lifecycle ---------- */
	{
		id: 'hostsvc',
		parent: 'host',
		title: 'Host service',
		sub: 'supervisor · telemetry · reports',
		what: 'The cas-host runtime around the images: validates endpoints, scans and recovers the store, attaches one frontend per image, supervises the socket threads every 10 ms, samples telemetry every 500 ms, and writes host.json plus one report per image.',
		how: ['Strict telemetry fails the run at 4,096 samples or 64 MiB; the interactive lab uses bounded rotation instead.', 'There is no control socket: control is the command line plus files.'],
		files: ['crates/cas/daemon/src/host_service.rs', 'crates/cas/daemon/src/host_service/telemetry.rs'],
		docs: ['host-service', 'pressure-telemetry']
	},
	{
		id: 'host-recovery',
		parent: 'host',
		title: 'Store recovery',
		sub: 'inspect everything · then repair',
		tone: 'outline',
		what: 'Opens a store for serving. It locks tickets, inspects the catalog, chunk store, every manifest and WAL and every snapshot read-only, validates that every root\'s chunks exist and every required prefix is covered, and only then repairs: archive rejected suffixes, truncate, sync, write recovery fences.',
		how: [
			'Cold: no guest survives; each WAL rotates to a new writer epoch and fences before serving.',
			'Retained: every frontend must first hand over its validated carrier state; the coordinator replays every image\'s missing mutations from surviving guest RAM, fences, and activates all images behind one barrier.',
			'One missing or incompatible image blocks the whole host: the dependency graph is shared.'
		],
		types: ['Inspection', 'Checked', 'Recovered', 'RetainedHost'],
		files: ['crates/cas/daemon/src/local/host/recovery.rs', 'crates/cas/daemon/src/local/host/recovery/frontend.rs', 'crates/cas/daemon/src/local/host/recovery/live.rs', 'crates/cas/daemon/src/local/host/initialize.rs'],
		docs: ['shared-recovery', 'shared-live-recovery', 'host-initialization']
	},
	{
		id: 'storage-enum',
		parent: 'host',
		title: 'Storage dispatch',
		sub: 'Raw · Staging v1 · Local · Opening',
		tone: 'muted',
		what: 'The enum the frontend talks to. Raw is a plain io_uring file backend and Staging the v1 serial log; both are kept as controls for the suite. Local is the runtime above; Opening is a locked, unrepaired image awaiting inflight negotiation.',
		how: ['The frontend still inspects the variant at several call sites to decide gather versus bounce copy and whether an error is fatal; the review asked for one submission entry point.', 'Restartable staging keeps the one-request-in-flight workaround from Update 01 and flushes after every write.'],
		types: ['Storage', 'Opening', 'Permit', 'Completed'],
		files: ['crates/cas/daemon/src/storage.rs', 'crates/cas/daemon/src/storage/opening.rs'],
		docs: ['rust-design']
	}
];
