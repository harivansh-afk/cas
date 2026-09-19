import type { SystemNode } from '../graph';

/** The guest, QEMU, the host kernel and the bytes on disk: everything around the two Rust crates. */
export const machine: SystemNode[] = [
	{
		id: 'guest',
		title: 'Guest VM',
		sub: 'Linux · unmodified',
		what: 'An ordinary Linux virtual machine. It sees one virtio-blk disk with 4 KiB blocks and never learns that the bytes behind it are content-addressed. Its own page cache, filesystem journal and fsync semantics are untouched.',
		how: [
			'Applications and fio write files; ext4 turns them into 4 KiB block requests and jbd2 journal writes; fsync becomes a virtio FLUSH.',
			'The guest keeps a private page cache. A CAS cache hit is still copied into guest RAM; no memory is shared between guests.',
			'In the lab the guests run under TCG emulation inside a KVM outer VM, so their timings are development measurements, not native ones.'
		],
		files: ['nix/guest/default.nix', 'nix/shared/guest.nix', 'nix/lab/guest.nix']
	},
	{
		id: 'guest-fs',
		parent: 'guest',
		title: 'Filesystem + page cache',
		sub: 'ext4 · jbd2 · fsync',
		what: 'The guest filesystem mounted on the CAS disk at /mnt/cas. It decides block layout, journaling and when a FLUSH is issued. Nearly everything it writes is 4 KiB aligned, which is why fixed 4 KiB chunks capture most duplicates.',
		how: [
			'A file write lands in the guest page cache and is written back later as block IO.',
			'fsync or a journal commit issues a virtio-blk FLUSH; the guest cannot proceed until CAS acknowledges it durable.',
			'A jbd2 journal write on a queue can sit ahead of unrelated reads on that queue; the read bypass below exists because of this.'
		],
		files: ['nix/shared/workload.sh', 'crates/harnesses/fio/live.fio']
	},
	{
		id: 'guest-blk',
		parent: 'guest',
		title: 'virtio-blk driver',
		sub: 'split virtqueues · 512-byte sectors',
		what: 'The guest kernel driver. It places 16-byte request headers, data buffers and a status byte into descriptor chains in guest memory, kicks a queue, and later consumes used entries.',
		how: [
			'Requests count in 512-byte sectors: sector 8 means byte 4096. CAS only accepts whole aligned 4 KiB blocks up to 1 MiB.',
			'With the shared host the device advertises four queues of 256 entries; the reference backends advertise one queue of 128.',
			'The driver must negotiate VERSION_1, BLK_SIZE and FLUSH or the backend refuses before any IO.'
		],
		numbers: [['queues · depth', '4 × 256 (shared host); 1 × 128 (reference)'], ['max request', '1 MiB, 16 segments of 64 KiB']],
		docs: ['vhost-user-notes']
	},
	{
		id: 'qemu',
		title: 'QEMU',
		sub: 'stock · vhost-user-blk-pci',
		what: 'The unmodified virtual machine monitor. It emulates the PCI device, shares guest RAM with the daemon and configures the virtqueues over a Unix socket. After setup it leaves the data path entirely.',
		how: [
			'Guest RAM is backed by a memfd or file with share=on so another process can map the same pages.',
			'On connect QEMU sends SET_MEM_TABLE with the RAM file descriptors, negotiates features and protocol features, then SET_VRING_* for each queue.',
			'Between GET_INFLIGHT_FD and SET_INFLIGHT_FD it retains the daemon\'s inflight memfd, which is what makes a daemon replacement possible without a reboot.',
			'QEMU 10.2.4 distinguishes stop/start (SET with the retained fd) from a full device reset (GET then SET).'
		],
		docs: ['vhost-user-notes', 'lifecycle'],
		files: ['crates/harnesses/src/qemu.rs', 'nix/run-vm.sh']
	},
	{
		id: 'qemu-socket',
		parent: 'qemu',
		title: 'vhost-user socket',
		sub: 'Unix socket + SCM_RIGHTS',
		what: 'The control channel between QEMU and the daemon. Every message is a small struct; file descriptors for guest memory, kick and call eventfds and the inflight carrier travel as ancillary data.',
		how: [
			'QEMU is the frontend (client); the daemon listens and accepts exactly one connection per image.',
			'Kick eventfds wake the daemon when the guest posts work; call eventfds interrupt the guest when a used entry is published.',
			'The socket must live on a different filesystem from the storage root so its inode never counts against the governed disk.'
		],
		docs: ['vhost-user-notes', 'host-service']
	},
	{
		id: 'qemu-ram',
		parent: 'qemu',
		title: 'Shared guest RAM',
		sub: 'descriptors · rings · payload',
		what: 'The guest\'s physical memory, mapped a second time inside the daemon. Descriptor tables, available and used rings and every data buffer live here, so a request is parsed and completed without copying it through the socket.',
		how: [
			'The daemon keeps its own accepted snapshot of the mapping and uses it for payload reads, status bytes and used-ring writes even while the framework replaces its atomic map.',
			'A write\'s payload is gathered straight from these pages into its final WAL buffer: one copy on the host.',
			'A read\'s response is copied back into these pages before the status byte is written.'
		],
		docs: ['shared-frontend', 'review/c2-copy-path']
	},
	{
		id: 'qemu-carrier',
		parent: 'qemu',
		title: 'Retained inflight memfd',
		sub: 'CASIFL03 · survives daemon death',
		what: 'A sealed memory file the daemon creates and QEMU keeps open. It records which requests were discovered and admitted, per queue head, plus the published prefix P. A replacement daemon reads it back to resume the same guest.',
		how: [
			'The standard vhost-user inflight regions come first; a CAS trailer of one 4 KiB header and one 128-byte slot per queue head follows.',
			'Slots move EMPTY → DISCOVERED → PREPARED → ACTIVE → EMPTY; a header FAILED flag freezes every transition except error completion.',
			'It holds identities and sequence numbers only, never payload: payload is either in guest RAM or in the WAL.'
		],
		numbers: [['magic · version', 'CASIFL03 · 3'], ['max mapping', '155,648 bytes (4 queues × 256)']],
		docs: ['inflight-format', 'storage-design']
	},
	{
		id: 'kernel',
		title: 'Host Linux',
		sub: 'NixOS · KVM · XFS · io_uring',
		what: 'The host kernel does the actual storage work: it runs the guest under KVM, executes io_uring submissions against O_DIRECT files on XFS, and provides the durability primitive (fdatasync) that every promise in the design rests on.',
		how: [
			'CAS never uses the host page cache for payload: every segment is opened O_DIRECT with 4 KiB alignment checked through statx.',
			'Reflinks, hole punching and FIEMAP make snapshots and reclamation cheap without moving live pages.',
			'The declared crash model: writes covered by a successful fdatasync survive; anything after may be lost, torn or reordered.'
		],
		docs: ['storage-design', 'testbed']
	},
	{
		id: 'xfs',
		parent: 'kernel',
		title: 'XFS + block layer',
		sub: 'O_DIRECT · fdatasync · FICLONE · punch',
		what: 'A dedicated XFS filesystem on the test disk holds the whole store. It is chosen for reflink support, direct IO alignment reporting and hole punching, all of which the design depends on.',
		how: [
			'open(O_DIRECT | O_NOFOLLOW | O_NONBLOCK) plus flock on every segment, manifest and catalog file; STATX_DIOALIGN must report 4 KiB alignment or the open fails rather than falling back to buffered IO.',
			'fallocate(KEEP_SIZE) preallocates segments so recovery can distinguish written length from reserved space; PUNCH_HOLE frees compacted WAL payload and dead manifest pages in place.',
			'ioctl(FICLONE) reflinks a manifest to make a snapshot; FIEMAP verifies nothing remains beyond EOF after trimming.',
			'fstatvfs on the root is the only source of truth for physical space; deleting a name is not counted until observed.'
		],
		files: ['crates/cas/core/src/direct.rs', 'crates/cas/core/src/direct/fiemap.rs', 'nix/modules/disks.nix'],
		docs: ['xfs-fixture', 'physical-space']
	},
	{
		id: 'io-uring',
		parent: 'kernel',
		title: 'io_uring',
		sub: 'one ring per image reactor',
		what: 'The asynchronous IO interface each reactor thread uses for WAL appends, fence writes, fdatasync, chunk and page reads, and eventfd polls. Submissions are ordered by the reactor, not the kernel.',
		how: [
			'A ring of 256 entries with a registered wake eventfd; the reactor reaps completions by token and rejects stale tokens from retired generations.',
			'An fdatasync is submitted only after the fence write it covers has completed, so no drain flag is needed on the host path; the raw reference backend uses IO_DRAIN instead.',
			'A PollAdd on another reader\'s eventfd is how a waiter joins an in-flight chunk fetch without a thread.'
		],
		files: ['crates/cas/daemon/src/local/reactor.rs', 'crates/cas/daemon/src/storage.rs'],
		docs: ['async-io']
	},
	{
		id: 'kvm',
		parent: 'kernel',
		title: 'KVM / TCG',
		sub: 'runs the guest CPU',
		what: 'Hardware virtualization for the guest. On Spark the lab runs one KVM outer VM that itself hosts the storage and the TCG-emulated inner guests, which is why every latency number so far is labelled a development measurement.',
		how: ['The host kernel has module loading disabled and no XFS driver, so the XFS test filesystem lives inside the KVM VM.', 'G1 requires a dedicated host and raw media before any p99 number counts.'],
		docs: ['testbed', 'ci-evaluation']
	},
	{
		id: 'ipc',
		parent: 'kernel',
		title: 'IPC primitives',
		sub: 'eventfd · memfd · flock · timerfd',
		what: 'The small kernel objects that carry control between threads and processes: eventfds for wakeups, memfds for shared memory, flock for exclusive ownership, timerfds for retry and recovery deadlines.',
		how: [
			'Every reactor, frontend and scheduler wake is an eventfd; a write of 1 that returns EAGAIN counts as already pending.',
			'flock on the actual file description, not PID death, is what proves an old daemon\'s kernel IO can no longer touch a segment.',
			'A monotonic timerfd wakes the frontend for the 100 ms admission retry and the 60 s recovery deadline even when the guest sends nothing.'
		],
		files: ['crates/cas/core/src/eventfd.rs', 'crates/cas/daemon/src/deadline.rs', 'crates/cas/daemon/src/inflight/mapping.rs']
	},
	{
		id: 'disk',
		title: 'Store on disk',
		sub: '<store-root>/ on one XFS filesystem',
		what: 'Everything durable lives under one root on a dedicated filesystem. Segment numbers come from one monotonic ticket namespace across chunk and staging segments, and the highest durable header is the allocation record: there is no counter file.',
		how: [
			'catalog/catalog.v2 names the images and snapshots that exist; loose files are not membership.',
			'chunks/ holds shared immutable chunk segments; images/<id>/ holds a private manifest and staging WAL per image; snapshots/<id>/ holds reflinked manifests.',
			'All integers are little-endian, every record is CRC32-checked, every IO offset is a 4 KiB multiple.',
			'Rejected or torn suffixes are archived under rejected/ before any destructive repair.'
		],
		docs: ['storage-format', 'storage-design', 'catalog']
	},
	{
		id: 'f-catalog',
		parent: 'disk',
		title: 'catalog/catalog.v2',
		sub: 'CASCAT02 · membership',
		what: 'One small file listing every image and snapshot with its committed root, durable D and geometry. It is published atomically: write pending-<gen>-<attempt>.v2, fsync, rename, fsync the directory.',
		how: ['64-byte header with a generation that increases on every change; 128-byte entries sorted by 16-byte id; whole-file CRC32.', 'Image entries carry image bytes; snapshot entries carry the source image plus the exact manifest commit and end they reflink.', 'Recovery reads only catalog.v2; pending files are never adopted or removed.'],
		files: ['crates/cas/core/src/catalog.rs', 'crates/cas/core/src/catalog/format.rs'],
		docs: ['catalog']
	},
	{
		id: 'f-chunks',
		parent: 'disk',
		title: 'chunks/segment-<ticket>.v2',
		sub: 'CASCHS02 · CASCHB02',
		what: 'Shared, immutable chunk segments. Each starts with a 4 KiB header, then batches of one 4 KiB header plus up to 63 chunks of exactly 4 KiB, each described by its BLAKE3 hash and CRC32. A chunk\'s address is its segment ticket and block index.',
		how: ['A full batch is 256 KiB; segments are preallocated to 64 MiB and may reach 256 MiB.', 'Ordinals are dense within a segment so a batch header\'s position is checkable against its number.', 'Collection copies live chunks into a fresh ticket and unlinks the old segment; the highest ticket is truncated to its header instead so the allocation record survives.'],
		files: ['crates/cas/core/src/store/format.rs', 'crates/cas/core/src/store/file.rs'],
		docs: ['storage-format', 'chunk-store-io']
	},
	{
		id: 'f-manifest',
		parent: 'disk',
		title: 'images/<id>/manifest.v2',
		sub: 'CASMAN02 · COW B+tree',
		what: 'One append-only file per image mapping logical blocks to chunk hashes. A 4 KiB FILE header, then pages: LEAF (63 extents), BRANCH (252 children) and COMMIT. Children always sit at lower offsets than parents, so any COMMIT names a complete tree.',
		how: ['Leaf extents are (start, end, hash, kind): kind 1 is one hashed block, kind 2 is a ZERO range. Gaps read as zeros.', 'A COMMIT binds store, image, generation, root offset and height, and D, the mutation sequence the tree covers.', 'Old pages become garbage once no pinned root reaches them; reclamation hole-punches them without moving anything.'],
		files: ['crates/cas/core/src/manifest/format.rs', 'crates/cas/core/src/manifest/file.rs'],
		docs: ['storage-format', 'manifest-editor']
	},
	{
		id: 'f-staging',
		parent: 'disk',
		title: 'images/<id>/staging/segment-<ticket>.v2',
		sub: 'CASSEG02 · CASBAT02 · private WAL',
		what: 'The image\'s write-ahead log: preallocated 64 MiB segments of packed batches. Each DATA batch is a 4 KiB header of up to 63 descriptors plus up to 1 MiB of whole-block payload; a FENCE is a header alone carrying the mutation boundary a FLUSH made durable.',
		how: ['The segment header records store and image identity, writer epoch, number, capacity and the mutation sequence preceding it, so recovery chains segments without a directory index.', 'Every descriptor carries the request\'s serial, mutation sequence, attachment, queue and head, which is what live replay matches against the carrier.', 'Compacted payload is punched out below D; the batch headers stay until the whole segment can be unlinked.'],
		numbers: [['isolated 4 KiB WRITE + FLUSH', '12 KiB on disk'], ['32 packed WRITEs + FLUSH', '136 KiB for 128 KiB payload']],
		files: ['crates/cas/core/src/append/format.rs', 'crates/cas/core/src/append/segment.rs'],
		docs: ['storage-format', 'wal-allocation']
	},
	{
		id: 'f-snapshots',
		parent: 'disk',
		title: 'snapshots/<id>/manifest.v2',
		sub: 'FICLONE of an exact root',
		what: 'A snapshot is a reflink of the source manifest cut at the exact end of one COMMIT. It shares extents with the source until either side changes, and its catalog entry makes it a garbage-collection root.',
		how: ['Created only under a host quiescence, after the image was compacted through the cut.', 'A writable clone reflinks a snapshot and appends one COMMIT with a new image identity and D = 0; the old COMMITs stay as history but no longer match.'],
		files: ['crates/cas/core/src/manifest/file/snapshot.rs', 'crates/cas/daemon/src/local/host/snapshots.rs'],
		docs: ['snapshot-files', 'host-snapshots']
	},
	{
		id: 'f-outside',
		parent: 'disk',
		title: 'Outside the store',
		sub: 'sockets · reports · telemetry',
		tone: 'muted',
		what: 'Things that must not sit on the governed filesystem: vhost-user sockets, host.json and per-image reports, telemetry.jsonl, compaction-pause markers and the harness result directories.',
		how: ['cas-host refuses an endpoint whose socket parent or reports directory shares st_dev with the storage root.', 'Reports are written with create_new so a rerun never overwrites evidence.'],
		files: ['crates/cas/daemon/src/host_service.rs', 'crates/cas/daemon/src/host_service/telemetry.rs']
	}
];
