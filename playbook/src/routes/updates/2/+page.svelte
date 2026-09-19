<script lang="ts">
	import { base } from '$app/paths';
	import '$lib/architecture/article.css';
	import Figure from '$lib/architecture/Figure.svelte';
	import Prefixes from '$lib/architecture/Prefixes.svelte';
	import Flow from '$lib/architecture/Flow.svelte';
	import Code from '$lib/architecture/Code.svelte';
	import Measurements from '$lib/architecture/Measurements.svelte';
	import Pressure from '$lib/architecture/Pressure.svelte';
	import ReadPressure from '$lib/architecture/ReadPressure.svelte';
	import ReadDecision from '$lib/architecture/ReadDecision.svelte';
	import ReadProgress from '$lib/architecture/ReadProgress.svelte';
	import { source, record, doc, revision, testedRevision } from '$lib/architecture/source';

	const congestion = doc('congestion-wait');
	const congestionResult = 'https://git.harivan.sh/harivansh-afk/cas-research/src/commit/fc2a28beb2659dcdd2726928e846355cb6bad32d/docs/measurements/pressure-repeat-2026-09-14/README.md';
	const cliSource = source;
	const pressureValidation = doc('validation/2026-09-14-pressure-repeat');
	const sections = [
		['stack', 'Who owns what'], ['write', 'Write and FLUSH'], ['reads', 'Find the latest bytes'],
		['compaction', 'Turn writes into shared chunks'], ['snapshots', 'Free space and take snapshots'],
		['resources', 'When writers outrun storage'], ['recovery', 'Recover after a crash'],
		['measurements', 'What the experiments show'], ['limits', 'What remains to solve'],
		['evidence', 'Evidence and source revisions'], ['try', 'Run it yourself']
	];
	const audit = source('docs/review/2026-09-14-storage-work.md');
	const tree = `One host kernel
├─ QEMU A → guest Linux A → private filesystem
├─ QEMU B → guest Linux B → private filesystem
└─ cas-host                         one process, cas-daemon crate
   ├─ socket / queue worker × image  parses virtio; owns guest completion
   ├─ image reactor × image         WAL / read IO; one write order
   └─ cas-compactor                  shared chunk writer; manifests; GC
      shared: hash index, read caches, budgets, catalog

The guests have separate kernels and page caches.
The reactors and compactor are threads in the same Rust process.`;
	const disk = `<store-root>/                      one dedicated filesystem
├─ catalog/catalog.v2               active images and snapshot roots
├─ chunks/segment-<ticket>.v2       shared immutable chunk batches
├─ images/<image-id>/
│  ├─ manifest.v2                   logical blocks → content hashes
│  └─ staging/segment-<ticket>.v2   private write-ahead log (WAL)
└─ snapshots/<snapshot-id>/
   └─ manifest.v2                   immutable reflink of an exact root

Outside this filesystem: vhost-user sockets and reports.
Loose pending files are not catalog membership.`;
</script>

<svelte:head>
	<title>Update 02 · How CAS works</title>
	<meta name="description" content="The single-host foundation for the CAS study: ownership, write and read paths, compaction, recovery, measured costs and open research questions. Includes source paths and a VM CLI." />
</svelte:head>

<article class="architecture" id="beginning">
	<header>
		<a class="back" href="{base}/">← index</a>
		<span class="eyebrow">Update 02 · 14 September 2026</span>
		<h1>Private disks, shared bytes</h1>
		<p class="lede">CAS gives each VM a private writable disk while sharing identical stored blocks across images. This update explains the working single-host backend: how writes become durable, how reads find their bytes, and how background work recovers space.</p>
		<p>The <a href="{base}/00/">study</a> asks whether sharing blocks by content can reduce storage and data movement across hosts enough to pay for indexing, copying and coordination. This backend lets us first measure those costs on one host. Remote reads, replication and migration remain to be built.</p>
		<p class="checkpoint"><strong>Latest finding:</strong> the merged scheduler lets independent reads pass blocked writes, and live recovery checks pass. Same-queue p99 is lower in this repeat, but multi-second outliers remain. Admission fairness is the next issue to resolve. <a href="#measurements">Measurements and design tradeoffs ↓</a></p>
	</header>

	<nav aria-label="Architecture contents"><ol>{#each sections as [id, title]}<li><a href={`#${id}`}>{title}</a></li>{/each}</ol></nav>

	<section id="stack">
		<h2>Each image owns its writes. One host shares the chunks.</h2>
		<p><strong>QEMU owns the virtual machine. <code>cas-host</code> owns its disk backend.</strong> Inside each guest, Linux turns file operations into block requests. CAS sees disk offsets and bytes; ext4 directories and SQLite transactions stay inside the guest.</p>
		<Figure name="owners" />
		<p>An <strong>image</strong> is one guest’s disk. Its <strong>write-ahead log (WAL)</strong> holds recent writes; its <strong>manifest</strong> maps settled blocks to content hashes. The shared <strong>chunk store</strong> holds the bytes named by those hashes.</p>
		<p>The <strong>frontend</strong> accepts guest requests and returns completions. The image’s <strong>reactor</strong> drives IO in one write order. One shared <strong>compactor</strong> turns durable WAL data into chunks and updates manifests. These run in <code>cas-host</code>; the storage formats and indexes live in <code>cas-core</code>.</p>
		<details id="processes"><summary>Processes, threads, crates and entry points</summary>
			<pre><code>{tree}</code></pre>
			<div class="table-scroll"><table class="spec"><thead><tr><th>Crate / directory</th><th>Responsibility</th></tr></thead><tbody>
				<tr><td><code>cas-daemon</code></td><td>Both <code>cas-host</code> and the older <code>cas-daemon</code> executable. Guest protocol, ordering, reactors, shared worker and recovery coordination.</td></tr>
				<tr><td><code>cas-core</code></td><td>WAL, chunks, indexes, manifests, catalog, file IO, resource accounting and scheduling primitives.</td></tr>
				<tr><td><code>cas-cli</code></td><td>The <code>casctl</code> CLI: VM commands, SSH, measurements and census. It calls the harness; it is outside the block IO path.</td></tr>
				<tr><td><code>cas-harness</code></td><td>Launches experiments, checks workloads, records source/artifact identity and verifies suites.</td></tr>
				<tr><td><code>crates/vendor</code></td><td>Pinned rust-vmm extensions for inflight FDs, lifecycle hooks and saved-head descriptor walking. QEMU remains stock.</td></tr>
				<tr><td><code>nix</code> / <code>experiments</code></td><td>Build and launch reproducible host/guest fixtures. These are not extra storage servers.</td></tr>
			</tbody></table></div>
			<p>Runtime coordination uses ordinary threads, bounded channels, mutexes and eventfds. The harness controls child process groups. The repository does not install per-image cgroup CPU, memory or IO limits.</p>
			<Code paths={['Cargo.toml', 'crates/cas/daemon/src/bin/cas-host.rs', 'crates/cas/daemon/src/host_service.rs', 'crates/cas/cli/src/main.rs', 'crates/harnesses/src/main.rs', 'crates/vendor/README.md', 'nix/shared/guest.nix', 'crates/harnesses/src/process.rs']} />
		</details>
		<h3 id="transport">The transport changes at each boundary</h3>
		<p>QEMU sets up the disk through a <strong>Unix vhost-user socket</strong> and passes file descriptors. Requests and payload live in <strong>shared guest RAM</strong>; kick/call eventfds announce work and completion. Inside CAS, channels move owned Rust values. Foreground disk IO uses <strong>io_uring</strong>; the compactor uses blocking file IO on its own thread.</p>
		<details id="operation-traces"><summary>Trace an operation across every boundary</summary>
			<Flow />
		</details>
		<details id="memory"><summary>Guest RAM, inflight memory and io_uring are different mappings</summary>
			<Figure name="memory" />
			<p>QEMU shares the guest-memory backing with CAS. <code>GuestMemoryMmap</code> gives the adapter checked access; another mapping does not copy all guest RAM. A KVM memory slot describes guest-physical address backing. It carries no block operation.</p>
			<p>The separate inflight memfd holds request identities and the published prefix. QEMU retains that FD across a backend crash. It contains no write payload. Host io_uring submission/completion rings are a third mapping, shared with the host kernel.</p>
			<p>Guest page caches remain private. A CAS cache hit still copies bytes into the requesting guest. The local runtime has no remote chunk transport or replication protocol.</p>
			<Code paths={['crates/cas/daemon/src/service.rs', 'crates/cas/daemon/src/request.rs', 'crates/cas/daemon/src/inflight/mapping.rs', 'crates/cas/daemon/src/local/reactor.rs']} />
			<p class="reference">Protocol reference: <a href="https://www.qemu.org/docs/master/interop/vhost-user.html">QEMU vhost-user</a>.</p>
		</details>
	</section>

	<section id="write">
		<h2>WRITE makes bytes visible. FLUSH makes them durable.</h2>
		<p>The frontend reserves capacity before assigning a mutation number. It gathers the guest’s bytes into the final aligned append buffer, then sends that buffer to the reactor. The reactor appends to the image’s <strong>write-ahead log (WAL)</strong>. Completed appends publish in order; a slow earlier write holds back later publication.</p>
		<p>A FLUSH captures a fixed write boundary, writes a fence and syncs it. Later write submissions wait outside that cohort, so a continuous writer cannot make one FLUSH chase an ever-growing stream. Reads and other images can continue during this ordinary sync.</p>
		<div class="prefix-key" id="durability"><span><strong>P</strong> published to the live view</span><span><strong>E</strong> synced in the WAL</span><span><strong>D</strong> committed in the manifest</span></div>
		<p>During normal serving, <code>D ≤ E ≤ P</code>. A guest WRITE can complete at P. Its FLUSH waits for E. Compaction later moves D forward.</p>
		<Prefixes />
		<Code paths={['crates/cas/daemon/src/backend.rs', 'crates/cas/daemon/src/local.rs', 'crates/cas/daemon/src/local/reactor.rs', 'crates/cas/core/src/append/submission.rs']} />
		<details id="wal"><summary>WAL layout, alignment and the actual copying cost</summary>
			<p>A v2 batch contains one 4 KiB header and up to 1 MiB of payload. The header holds up to 63 operation descriptors; a FENCE occupies its own 4 KiB header. Thus one isolated 4 KiB WRITE plus FLUSH encodes 12 KiB; 32 packed 4 KiB WRITEs plus FLUSH encode 136 KiB. This is format arithmetic, excluding segment creation, filesystem metadata and compaction.</p>
			<p>Batching uses available work: it seals at capacity, FLUSH or the end of the queue-drain pass. There is no batching timer holding a lone write. CRC32 protects framing and payload. Recovery follows validated boundaries rather than scanning arbitrary payload for fence-like bytes.</p>
			<p>The guest sector field counts 512-byte sectors: sector 8 means byte 4096. Accepted payload IO must cover whole aligned 4 KiB blocks, up to 1 MiB per request. Unsupported geometry is rejected. Multi-block requests have no whole-request atomicity guarantee.</p>
			<p id="direct"><code>O_DIRECT</code> bypasses the ordinary host payload page cache. CAS still uses XFS, the host block layer and device driver. It checks <code>STATX_DIOALIGN</code>; it does not silently fall back to buffered IO. <code>fdatasync</code> supplies the durability boundary; namespace publication also syncs directories.</p>
			<p>“One gather” describes guest-to-WAL-buffer copying. Compaction later reads and copies surviving bytes into chunk output. Short IO fails its owner; it is not retried as an arbitrary suffix. The 50 ms idle-sync setting is a policy trigger, not a hard durability deadline.</p>
			<Code paths={['crates/cas/core/src/append/format.rs', 'crates/cas/core/src/append/recovery.rs', 'crates/cas/core/src/direct.rs', 'crates/cas/daemon/src/request.rs']} />
			<p class="reference">Linux contracts: <a href="https://man7.org/linux/man-pages/man2/open.2.html">open(2)</a>, <a href="https://man7.org/linux/man-pages/man2/fsync.2.html">fsync(2)</a>. Full <a href={doc('storage-format')}>byte format</a>.</p>
		</details>
	</section>

	<section id="reads">
		<h2>Recent writes take precedence over shared chunks</h2>
		<p>The image’s staging index first supplies its newest published writes or ZERO ranges. Only uncovered blocks consult the manifest and shared chunk index below. A read pins its chosen files and manifest root, so compaction cannot invalidate its sources halfway through.</p>
		<div class="lookup-key"><div><strong>Image manifest</strong><span>logical block → content hash</span></div><span aria-hidden="true">↓</span><div><strong>Shared chunk index</strong><span>content hash → segment + offset</span></div></div>
		<p>Manifest content uses a shared clean chunk cache. On a miss, one reader fetches and verifies the hash; concurrent requests for that hash join its result. A separate manifest-page cache avoids repeated tree-page IO. Optional cache admission can fail while the verified read still succeeds.</p>
		<Code paths={['crates/cas/core/src/append/read.rs', 'crates/cas/core/src/manifest/tree/lookup.rs', 'crates/cas/core/src/chunk_index.rs', 'crates/cas/daemon/src/local/reactor/read.rs']} />
		<details><summary>Read IO, cache ownership and cold-read limitations</summary>
			<Figure name="lookup" />
			<p>WAL reads verify the original write CRC. A partial read may need scratch for that entire original payload. On the manifest path, the current reactor walks one block at a time: uncached page lookup, chunk header, then chunk payload. Multiple requests can overlap, but a single cold 1 MiB read does not submit all 256 chunk fetches at once.</p>
			<p>The clean cache defaults to 256 MiB of payload and uses ordinary LRU. It has no per-image cache partition or scan-resistant policy. Only verified read fills populate it; writes and compaction do not prewarm it. The 16 MiB manifest-page payload cache consumes part of the 128 MiB foreground metadata budget.</p>
			<p>Coalesced misses use bounded leader/waiter cells and eventfds; a reactor waits with io_uring <code>PollAdd</code>. The fetched buffer retains its original read credits through the final reader. Cache insertion makes a separate charged copy. Eviction drops cache membership, while an existing reader can retain the allocation.</p>
			<p>Page keys bind a registry incarnation, committed end and offset. A new file/root cannot inherit stale cache meaning. Guest RAM, shared mappings, retained fetches and cache payload are different owners; summing every report repeats some of the same memory.</p>
			<Code paths={['crates/cas/core/src/cache.rs', 'crates/cas/core/src/cache/lru.rs', 'crates/cas/core/src/cache/fills.rs', 'crates/cas/core/src/manifest/file/cache.rs']} />
		</details>
		<details id="manifests"><summary>The on-disk files and their durable owners</summary>
			<pre><code>{disk}</code></pre>
			<p>The manifest is an append-only copy-on-write B+tree of checksummed 4 KiB pages. Leaves contain extents; a COMMIT binds identity, root, geometry and D. <code>Manifest</code> owns mutation; <code>View</code> pins one committed root. It is not a content-hashed Merkle tree.</p>
			<p><code>Store</code> owns chunk output. Its shared <code>Reader</code> resolves immutable reads. The in-memory hash index is rebuilt from verified chunk records on recovery. Each address packs a 48-bit segment ticket and 16-bit block index. Moving a chunk changes that index, so GC need not rewrite every manifest.</p>
			<p><code>Catalog</code> defines complete image/snapshot membership. Publication syncs dependencies, writes and syncs a temporary catalog, renames it, then syncs its directory. <code>Tickets</code> allocates identities across chunk files, WALs and rejected names; reclamation preserves the highest durable identity to prevent reuse.</p>
			<Code paths={['crates/cas/core/src/manifest/file.rs', 'crates/cas/core/src/manifest/tree/editor.rs', 'crates/cas/core/src/store/file.rs', 'crates/cas/core/src/catalog.rs', 'crates/cas/core/src/segments.rs']} />
		</details>
	</section>

	<section id="compaction">
		<h2>The compactor converts settled writes into shared content</h2>
		<p>The compactor takes a bounded, durable WAL prefix above D. It drops versions fully covered by later edits in that selection, verifies the input and hashes each surviving nonzero <strong>4 KiB block with BLAKE3</strong>. Existing hashes reuse stored chunks; missing hashes get written and synced.</p>
		<Figure name="compact" />
		<p>Next it commits the image’s new manifest. Only then may the reactor install that committed view, advance D and reclaim covered WAL space once readers and recovery no longer need it. This order prevents a durable mapping from naming unsynced content. A newer guest overwrite remains in staging and keeps precedence.</p>
		<p>Identical blocks share even when their images have no common ancestor. A one-byte edit creates a new 4 KiB chunk. Duplicate writes still consume private WAL space first; deduplication does not remove foreground admission costs.</p>
		<Code paths={['crates/cas/daemon/src/local/host/worker.rs', 'crates/cas/core/src/append/compaction.rs', 'crates/cas/core/src/append/compaction/output.rs', 'crates/cas/core/src/store/file/insert.rs']} />
		<details><summary>Selection limits, scheduling and why this is fixed chunking</summary>
			<p>A validated segment/offset cursor resumes selection above the published D instead of rereading old framing. Recovery reconstructs that hint; a missing retained segment falls back to scanning. Manifest preparation grows as pages are needed, then emits only nodes reachable from the final root. Draft edits still update paths sequentially.</p>
			<p>One selection covers whole batches with at most 1 MiB of payload and 318 mapping edits. It prepares the complete copy-on-write edit before chunk publication. Partial ZERO overlaps remain ordered; splitting them could exceed the reserved edit bound. Chunk output batches contain at most 63 blocks.</p>
			<p>The worker uses typed selection, publication and reclamation replies to each reactor. The host gate precedes the image gate; those locks protect state publication and failure, not disk IO. A timed-out caller cannot refund the running worker’s buffers, files or physical-space promises.</p>
			<p>Work prefers 100 ms of settling; a 1 s age trigger and staging/index pressure force progress attempts. A continuously writing guest can trigger a finite fence to supply durable input. These timers do not bound completion when storage is slow.</p>
			<p>BLAKE3 chooses content identity. Boundaries are fixed logical blocks; content-defined chunking (CDC) remains unimplemented. All-zero content becomes a hole. The index compares full hashes, without byte-comparing incoming duplicates against disk. Compression, encryption and indexing payload in its original WAL location are absent from this path.</p>
			<Code paths={['crates/cas/core/src/chunk.rs', 'crates/cas/core/src/manifest/tree/editor.rs', 'crates/cas/core/src/aligned.rs', 'crates/cas/daemon/src/local/host.rs', 'crates/cas/daemon/src/local/state.rs', 'crates/cas/core/src/append/reclaim.rs']} />
		</details>
	</section>

	<section id="snapshots">
		<h2>Freeing space takes more than deleting a mapping</h2>
		<p>There are three kinds of space to recover. <strong>WAL reclamation</strong> frees old private write payload after compaction. <strong>Chunk GC</strong> traces content still reachable through images, snapshots and readers. <strong>Manifest sweeping</strong> frees obsolete mapping pages.</p>
		<div class="table-scroll"><table class="spec"><thead><tr><th>Storage</th><th>How space is released</th></tr></thead><tbody>
			<tr><td>Private WAL</td><td>Punch eligible payload; unlink retired segments after readers and replay identities release them.</td></tr>
			<tr><td>Shared chunks</td><td>Mark live content, copy live chunks out of selected segments, sync the copies, then delete the old segments.</td></tr>
			<tr><td>Manifest pages</td><td>Mark pages reachable from retained roots, then punch the gaps.</td></tr>
		</tbody></table></div>
		<p><strong>Ordinary compaction runs alongside guest IO. Host GC pauses new requests across all images.</strong> The shared worker drains admitted requests and fences the images before collection. Its file reads, writes, syncs, punches and unlinks run on that worker thread.</p>
		<p>Snapshots use the same host pause. The target is compacted through its fenced cut, its exact manifest is reflinked, and catalog membership is made durable before success. A core clone starts with that mapping but gets its own image identity, WAL and sequence namespace.</p>
		<p>This is a storage snapshot. It does not quiesce guest applications or make several images one atomic application transaction. Snapshot/clone primitives exist in Rust; the executable currently exposes initialization and serving, without a general live create/clone/delete/snapshot control API.</p>
		<Code paths={['crates/cas/daemon/src/local/host/collection.rs', 'crates/cas/daemon/src/local/host/snapshots.rs', 'crates/cas/core/src/store/file/collection.rs', 'crates/cas/core/src/manifest/file/snapshot.rs']} />
		<details><summary>Reclamation limits and ZERO / DISCARD</summary>
			<p>Compaction cannot free every covered WAL allocation immediately. Readers pin original payload, retained replay needs operation identity, and the current/highest segment may need to keep its header. The governor accounts allocation still held after punching or unlinking.</p>
			<p>Manifest sweeping walks each retained root once, collects page offsets under the metadata budget, then sorts and deduplicates them before punching gaps. Marking finishes before any page is punched. Memory scales with retained tree visits; allocation denial can still stop collection. The operation remains synchronous, and the current COMMIT keeps the file’s logical end high.</p>
			<p>Snapshots retain reachable chunks and may share manifest extents through XFS reflinks. Neither logical deletion nor a requested punch count equals measured physical savings.</p>
			<p>Negotiated WRITE_ZEROES and DISCARD both produce ZERO mutations, limited to one aligned range of at most 1 MiB. They read as zeros immediately after publication; physical release waits for later reclamation. Empty ranges consume no mutation number.</p>
			<Code paths={['crates/cas/core/src/append/reclaim.rs', 'crates/cas/core/src/manifest/file/reclaim.rs', 'crates/cas/daemon/src/backend/zero_tests.rs', 'crates/cas/daemon/src/request.rs']} />
		</details>
	</section>

	<section id="resources">
		<h2>A full WAL makes new writes wait</h2>
		<p><strong>Before admission, a waiting write stays in guest memory.</strong> CAS retains its descriptor without assigning a mutation sequence or copying its payload. After admission, a bounded host buffer carries it to the on-disk WAL; compaction drains that WAL in the background. The disk holds accepted writes, not an unlimited overflow queue.</p>
		<p>A writer can fill its WAL faster than the compactor drains it. <a href={congestion}>Healthy capacity waits stay pending until space returns.</a> Reclaimed capacity wakes admission; a retry timer covers releases without a notification. Independent reads can now pass a blocked write on the same virtqueue. Overlapping ranges and FLUSH barriers still constrain admission. <a href="#measurements">The latest experiment shows that distinction.</a></p>
		<details><summary>Which limit stops which work</summary>
		<div class="table-scroll"><table class="spec"><thead><tr><th>Pressure</th><th>Response</th><th>What still progresses</th></tr></thead><tbody>
			<tr><td>Request / byte credits</td><td>Wait before admission; final release wakes waiting images.</td><td>Other eligible images and the separate control reserve.</td></tr>
			<tr><td>WAL / staging index</td><td>Fence a finite prefix, compact/reclaim, rotate through the worker.</td><td>Eligible reads and covered FLUSHes; queue ordering can still block later requests.</td></tr>
			<tr><td>Physical space</td><td>Preserve the progress reserve; collect the host.</td><td>Already admitted owners drain. New guest IO pauses during collection.</td></tr>
			<tr><td>Live data cannot shrink</td><td>Report exhausted capacity; keep physical write admission closed.</td><td>Reads after a successful collection releases its host pause.</td></tr>
			<tr><td>Chunk-index memory</td><td>New unique chunks can fail compaction and its image.</td><td>No index spill or automatic memory-pressure GC fallback exists.</td></tr>
			<tr><td>IO timeout / integrity error</td><td>Close the affected failure gate; preserve uncertain owners.</td><td>An activated image socket failure is isolated. Shared corruption and incomplete retained recovery still stop all endpoints.</td></tr>
		</tbody></table></div>
		<p>Staging starts pressure compaction at 75% of its allocation-plus-promise cap, stops admission at the cap, and resumes below 60%. Physical space has a separate stop boundary that preserves the background reserve. GC must observe real filesystem release; deleting a name is insufficient.</p>
		<p>Admission keeps FIFO order among eligible heads within an image and uses byte deficit round robin across images, with a 1 MiB quantum. A second scheduler gives compaction one in four ready bulk submission opportunities; either side can borrow unused turns. This controls submission opportunities; it guarantees neither device bandwidth nor p99 latency.</p>
		</details>
		<Code paths={['crates/cas/daemon/src/backend/frontier.rs', 'crates/cas/daemon/src/backend/admission.rs', 'crates/cas/daemon/src/local/window.rs', 'crates/cas/daemon/src/local/host/fair.rs', 'crates/cas/core/src/scheduler.rs', 'crates/cas/core/src/space.rs']} />
		<details id="ownership"><summary>Budgets, ownership and deadlines in concrete terms</summary>
			<div class="table-scroll"><table class="spec"><thead><tr><th>Default limit</th><th>Per image</th><th>Shared host</th></tr></thead><tbody>
				<tr><td>Write requests</td><td>128</td><td>1,024</td></tr>
				<tr><td>Read requests</td><td>8</td><td>64</td></tr>
				<tr><td>Control reserve</td><td>8 / 64 KiB</td><td>32 / 256 KiB</td></tr>
				<tr><td>Append bytes</td><td>8 MiB</td><td>64 MiB</td></tr>
				<tr><td>Read/fetch bytes</td><td>8 MiB</td><td>64 MiB</td></tr>
				<tr><td>Metadata</td><td>Shares host budgets</td><td>128 MiB foreground + 128 MiB compaction</td></tr>
				<tr><td>Staging allocation</td><td>256 MiB</td><td>1 GiB</td></tr>
			</tbody></table></div>
			<p>These are code defaults, not a measured RSS total. <code>Budget</code> accounts capacity; <code>Share</code> imposes an image sublimit; leases follow owners. Budgeted collections account table capacity and old/new growth overlap. A read reserves response bytes plus 1 MiB of scratch allowance: the default byte budget admits seven simultaneous 4 KiB reads. Each frontend also reserves 6,297,600 bytes of metadata quota for descriptor snapshots; actual allocation depends on the discovered descriptors.</p>
			<p>The default staging geometry permits at most nine catalog images: their 64 MiB current segments must total strictly less than 60% of the 1 GiB host staging cap. Increasing that cap changes this startup constraint; it does not establish tested concurrency at the larger image count.</p>
			<p>The physical reserve is <code>R = 3S + M + 16 MiB</code>: 336 MiB with 64 MiB segments and a 128 MiB manifest transaction cap. One background owner may use it. The governed filesystem excludes unrelated writers; reports and sockets must live elsewhere.</p>
			<p>Queued bulk IO keeps its bounded buffer until a submission turn is available. Device IO retains its 30 s deadline; a shared-fetch eventfd poll waits for its leader instead. Explicit terminal drain has a separate 30 s deadline and cancels dependency polls without dropping uncertain owners. Recovery bounds waiting at 60 s. These values are not hard bounds on every cleanup or background syscall.</p>
			<p>Host collection pauses all admission, including controls from guests. Internally reserved fence/control work drains what was already accepted. Independent reads can pass a WAL-blocked write on the same or another virtqueue. Reads still wait for earlier overlapping work, FLUSH barriers and publication dependencies after admission. A control reserve does not bypass ordering dependencies.</p>
			<Code paths={['crates/cas/core/src/budget.rs', 'crates/cas/core/src/budget/allocator.rs', 'crates/cas/core/src/budget/shared.rs', 'crates/cas/core/src/budget/channel.rs', 'crates/cas/daemon/src/local/pools.rs', 'crates/cas/daemon/src/local/host/capacity.rs', 'crates/cas/core/src/space/filesystem.rs', 'crates/cas/daemon/src/deadline.rs']} />
		</details>
	</section>

	<section id="recovery">
		<h2>Recovery depends on which machine state survived</h2>
		<div class="table-scroll"><table class="spec"><thead><tr><th>Event</th><th>Recovery contract</th></tr></thead><tbody>
			<tr><td>CAS dies; QEMU lives</td><td>Retain guest RAM, rings and inflight FDs. Validate saved P and every image, replay missing owned mutations with original identities, then fence/sync before serving.</td></tr>
			<tr><td>QEMU is gone / fresh boot</td><td>Cold recovery uses validated catalog, manifests, chunks and WAL. Complete unsynced writes may survive; only synced durability is promised.</td></tr>
			<tr><td>Device reset / memory replacement</td><td>Drain old queue, memory and IO owners before accepting replacement state.</td></tr>
			<tr><td>Physical host power loss</td><td>Only stable storage survives. The sync contract is the model; this has not been established by a physical power-cut test.</td></tr>
		</tbody></table></div>
		<p>Retained recovery is coordinated across the catalog. One missing or incompatible image can prevent the whole shared host from resuming. This is a consequence of repairing a shared dependency graph under one owner.</p>
		<Code paths={['crates/cas/daemon/src/local/host/recovery.rs', 'crates/cas/daemon/src/local/host/recovery/frontend.rs', 'crates/cas/daemon/src/local/host/recovery/live.rs', 'crates/cas/daemon/src/inflight.rs']} />
		<details><summary>Replay identity, rejected tails and pinned library changes</summary>
			<Figure name="recovery" />
			<p>The replacement acquires real storage locks; PID death does not establish that old kernel IO released its files. It inspects every required dependency before repair. Published-but-unsynced mutations must already be covered by the valid WAL/manifest prefix; completed guest buffers may have been reused.</p>
			<p>The version-3 inflight carrier distinguishes discovered descriptors from admitted mutations. Discovered requests return to admission; they are not replayed as writes. Upgrading a version-2 retained attachment requires stopping its guest and making a fresh attachment. Still-owned missing mutations can be gathered from surviving guest RAM. Replay uses saved descriptor heads, original serials and mutation sequences. An old available-ring slot may already have wrapped, so replay cannot infer identity from that slot alone.</p>
			<p>All recovery fences finish before the common serving barrier opens. Cold recovery archives rejected suffixes before repair and establishes a new durable writer epoch. Corruption of required data is an error, not permission to silently roll back to an older convenient prefix.</p>
			<p>The vendored backend exposes inflight FD messages and brackets queue/memory lifecycle changes. The queue patch exposes descriptor walking from a saved head. These extensions are required by recovery; removing them as wrapper code would remove behavior tested by C3–C5.</p>
			<Code paths={['crates/cas/core/src/append/recovery.rs', 'crates/cas/core/src/append/shared.rs', 'crates/cas/daemon/src/backend/recovery.rs', 'crates/cas/daemon/src/backend/lifecycle.rs', 'crates/vendor/README.md']} />
		</details>
	</section>

	<section id="measurements">
		<h2>Read bypass helps, but the long tail remains</h2>
		<p>The merged read scheduler has now run through live QEMU recovery/reset checks and the same mixed workload. The results separate working bypass from the remaining scheduling delay. The study’s benefit over ZFS or across hosts remains unmeasured.</p>
		<ReadProgress />
		<details><summary>Earlier: tracing the original queue-head stall</summary><ReadDecision /></details>
		<details><summary>Earlier: locating the admission stall inside CAS</summary><ReadPressure /></details>
		<details><summary>Earlier: less compaction work and automatic write resumption</summary>
		<p>The preceding one-vCPU repeat measured the compaction changes and reproduced 10.5-second reads. It motivated the request-level instrumentation above.</p>
		<Pressure />
		<p>The code now resumes WAL selection from a validated cursor, grows preparation buffers as needed and writes only pages reachable from the final manifest root. The comparison measures the resulting reduction in work per compacted MiB. Buffer initialization counts are cumulative work, not peak RAM or deduplication savings.</p>
		<Code paths={['crates/cas/core/src/append/compaction.rs', 'crates/cas/core/src/aligned.rs', 'crates/cas/core/src/manifest/tree/editor.rs']} />
		<details><summary>Lab geometry, memory and measurement limits</summary>
			<p>Spark ran one 4 GiB KVM VM with a 4 GiB XFS lab disk and two inner 512 MiB, one-vCPU guests using TCG emulation. The service had a 6 GiB memory limit and no swap allowance. Its whole-service peak was 2.81 GiB; sampled storage PSS peaked at 441.90 MiB. These overlap and must not be added.</p>
			<p>There were no cgroup OOM events or limit hits. This does not test actual OOM or unexpected filesystem ENOSPC. No GC occurred in that run, so its read stalls cannot be attributed to a GC pause. One repetition with different backlog and timing does not establish a statistical speedup or sustained device rate.</p>
			<p>The lab disk and temporary keys were deleted after verification. Small reports and receipts remain. <a href={pressureValidation}>Commands, revisions, verification and cleanup</a>.</p>
		</details>
		</details>
		<details id="baseline"><summary>Earlier baseline · 13 September · CAS versus two raw backends</summary>
			<p>These measurements predate the congestion and compaction changes. At <code>e15d733</code>, each arm had two guests, one active and one idle. Controls used QEMU raw storage or the reference raw io_uring daemon.</p>
			<Measurements />
			<p>CAS’s median fdatasync p99 was 9.76 ms, versus 3.62 ms raw and 3.78 ms with the reference daemon. Sequential reads reached 437 MiB/s, versus 842 MiB/s raw. These observations motivated profiling; a new matched comparison is needed to describe the current backend.</p>
			<p>All 60 timed jobs passed. CAS completed 487 compaction batches, with no GC or cache eviction. Its lab peaked at 2,770 MiB of cgroup memory. No lab recorded an OOM kill.</p>
			<p>CAS advertised four queues and the controls one; each guest had one vCPU and jobs ran at QD1. Caches and telemetry remained enabled. These nested-lab results do not establish native NVMe performance or the study’s latency gate.</p>
			<p>Each plotted value is the median of five per-run results. The fdatasync p99 measures sync separately from the preceding write; adding their p99s would not yield a transaction p99. Process PSS/RSS was sampled every 500 ms; the service’s memory peak also includes the outer VM.</p>
			<p><a href={doc('measurements/casctl-2026-09-13/README')}>Method, per-run values and evidence receipts</a>.</p>
			<Code paths={['crates/harnesses/src/lab/bench.rs', 'crates/harnesses/src/lab/samples.rs', 'crates/cas/daemon/src/host_service/telemetry.rs']} />
		</details>
	</section>

	<section id="limits">
		<h2>What remains before the study can draw conclusions</h2>
		<ul class="findings">
			<li><strong>Memory bounds are incomplete.</strong> Report construction, framework allocations, thread stacks and final resident-memory attribution remain outside the closed audit. Passing budget counters cannot close that gap.</li>
			<li><strong>GC can stall every guest.</strong> Its sweep is synchronous, and deadline checks do not interrupt each blocking syscall or tree walk. Background watchdogs and guest-side timeouts still apply. Pause duration after a long overwrite history needs measurement.</li>
			<li><strong>The index limits unique capacity.</strong> Every stored hash needs RAM, including entries awaiting GC. Table growth overlaps old and new allocations. Disk headroom alone does not guarantee another unique write can be compacted.</li>
			<li><strong>Shared failures still stop the host.</strong> An image socket failure after activation now leaves its peers running; a protocol test checks that boundary. Shared corruption and incomplete retained recovery remain host-wide failures. There is no automatic image recovery.</li>
			<li><strong>One worker couples the images.</strong> Chunk writes, rotation, snapshots and collection share that thread. A stalled background operation delays the others. The read cache is also shared LRU, so a scan can displace another guest’s hot blocks.</li>
			<li><strong>Read isolation remains incomplete.</strong> Bounded bypass, overlap rules and retained recovery are implemented. Live mixed runs still show seconds-long fairness waits. The scheduler must preserve an eligible read’s opportunity when blocked writers retry.</li>
		</ul>
		<p>The current system is single-host storage with one writable attachment per image. Remote reads, replicated durability, migration/handoff, RDMA, CDC, compression, guest-memory sharing and general online lifecycle tooling remain outside the implemented path.</p>
		<p><strong>Next:</strong> resolve the shared admission fairness interaction and repeat the same mixed workload. Then attribute remaining reclamation reads/syncs and long-history GC pauses; finish the allocation audit and actual capacity-failure tests. Dedicated-media, ZFS and two-host comparisons are still required to answer the research questions. <a href={congestionResult}>Compaction measurements and code paths</a> · <a href={audit}>Full storage audit</a>.</p>
	</section>

	<section id="evidence">
		<h2>Development checks establish behavior at specific revisions</h2>
		<p>The records cover write ordering, durable FLUSH, compaction crash cuts, shared recovery and fresh-boot integrity. They support using this backend for further experiments. Dedicated-media performance, physical power loss and complete resident-memory accounting remain unaccepted.</p>
		<details><summary>Tested revisions, checkpoint counts and open gates</summary>
			<p>Architecture links use <a href={source('')}>{revision}</a>, the merged read scheduler tested here. Each older experiment keeps its own source revision; its timings do not describe this binary. Source links require Forgejo repository access.</p>
			<div class="table-scroll"><table class="spec"><thead><tr><th>Record</th><th>What it establishes</th></tr></thead><tbody>
				<tr><td><a href="https://git.harivan.sh/harivansh-afk/cas-research/src/commit/e741b4df78710ecbdead555af8d8d6dde2d5f834/docs/validation/2026-09-14-read-progress-live.md">14 September merged read scheduler</a></td><td>13 live recovery/reset scenarios, 50 workload jobs and eight full seed checks on <code>4b50554</code>. Bypass occurs; a new two-image Rust reproduction confirms remaining admission starvation. Latency acceptance stays open.</td></tr>
				<tr><td><a href="https://git.harivan.sh/harivansh-afk/cas-research/src/commit/02fa6e8eb9b74d3e91d7a4f360adda4fd44485b0/docs/validation/2026-09-14-read-pressure-trace.md">14 September read attribution</a></td><td>24 fio jobs on <code>fc738c2</code>; traced same-queue waits and separate-CPU controls. A subsequent observation-race correction passed 477 native tests; 25 fixture-dependent ignores. Scheduler behavior is unchanged.</td></tr>
				<tr><td><a href={pressureValidation}>14 September pressure repeat</a></td><td>20 fio jobs, quota resumption, drain and restart verification on <code>eeff4b5</code>. Same-queue read stalls remain.</td></tr>
				<tr><td><a href={doc('validation/2026-09-14-storage-work')}>14 September storage follow-up</a></td><td>471 native tests passed; 25 fixture-dependent tests were ignored in that run. Separately, 185 selected XFS tests and two-image live recovery passed. The record gives each tested source.</td></tr>
				<tr><td><a href={doc('validation/2026-09-13-c5-final')}>13 September integrated run</a></td><td>41/41 scenarios on <code>{testedRevision}</code>, including 178 XFS fixture tests, six compaction crash cuts, eleven competing-guest stages and fresh-boot checks. This predates the pressure fixes.</td></tr>
			</tbody></table></div>
			<p>C1–C3 cover the reference, copy/format and ordering/recovery checkpoints. C4’s 40 functional scenarios and C5’s integrated inventory passed, but aggregate closure still requires the final allocation audit and dedicated-media repetitions. The study’s G1–G6 research gates are separate.</p>
			<p>The suite verifier is a separate repository command. Scenario counts include build/style checks and nested groups; 41 scenarios are not 41 independent fault models. Earlier failures and retries remain in their session records.</p>
			<p>Bulk VM disks were removed after verification. Logs and receipts remain at the locations recorded by each session; they are not a portable archive of the deleted data. This editorial update runs no new storage experiment.</p>
			<Code paths={['crates/harnesses/src/suite/scenarios.rs', 'crates/harnesses/src/suite.rs', 'crates/harnesses/src/shared/pressure/checks.rs', 'crates/harnesses/src/persistence/oracle.rs', 'nix/shared/outer.nix']} />
		</details>
		<p><a href={doc('review/c4-c5-allocation-gaps')}>Open allocation audit</a> · <a href={doc('validation')}>Validation history</a> · <a href={record('TODO.md')}>Progress tracker</a></p>
		<details id="checkpoint"><summary>What changed since Update 01</summary>
			<p><a href="{base}/updates/1/">Update 01</a> describes the earlier fixed 8 KiB log and serial write-through recovery. The current host uses a packed WAL, concurrent retained recovery, a shared compactor and verified read caches. The current limits and source revisions are recorded above.</p>
		</details>
	</section>

	<section id="try">
		<h2>Start a VM. Write to its disk. Reopen it.</h2>
		<pre><code>casctl new demo --count 2
casctl ls
casctl shell demo
casctl ssh demo/2 -- lsblk
casctl status demo
casctl bench demo --case flush
casctl stop demo
casctl start demo
casctl stop demo
casctl rm demo</code></pre>
		<p><code>demo/1</code> and <code>demo/2</code> have private ext4 disks at <code>/mnt/cas</code>, backed by one shared CAS store. <code>stop</code> retains their data. <code>rm</code> deletes the stopped lab’s disk and archives its small results. The guest root is temporary.</p>
		<details><summary>Setup, transport and ownership</summary>
			<p>On Spark, build with <code>nix build .#casctl --out-link result-cli</code>. Use <code>./result-cli/bin/casctl</code> in place of <code>casctl</code> above, starting with <code>doctor</code> to check prerequisites. A user service owns each lab after the command exits. The CLI generates an SSH key locally and pins guest host keys; the private key stays on Spark.</p>
			<p>The lab puts a dedicated XFS filesystem and <code>cas-host</code> inside a 4 GiB KVM VM. Its inner guests use TCG emulation, 512 MiB each. SSH reaches them through a loopback port and the outer host. Block IO uses the same shared-memory virtqueues, eventfds and io_uring paths described above.</p>
			<p>Interactive telemetry retains the current and previous files through bounded rotation. Strict checkpoint recording instead fails at 4,096 samples or 64 MiB.</p>
			<p>The backing disk is capped at 4 GiB. The user service has a 6 GiB memory limit, and the runner checks for 25 GiB of remaining host disk space. Guest count is chosen at creation; separate <code>new</code> commands create separate stores.</p>
			<p><a href={cliSource('docs/casctl.md')}>Commands and limits</a> · <a href={cliSource('crates/cas/cli/src/main.rs')}>crates/cas/cli/src/main.rs</a> · <a href={cliSource('crates/harnesses/src/lab.rs')}>crates/harnesses/src/lab.rs</a> · <a href={cliSource('nix/lab/host.nix')}>nix/lab/host.nix</a></p>
		</details>
	</section>

	<footer id="source-map"><a href="{base}/">← index</a><a href="#beginning">↑ beginning</a><a href={audit}>full source audit ↗</a></footer>
</article>
