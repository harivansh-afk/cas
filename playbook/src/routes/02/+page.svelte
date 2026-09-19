<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="02" />
<p class="lede">
	The R0–R3 comparison runs the same stock QEMU, the same guest, and the same NVMe device, and varies only the storage behind the device.<br />
	Hypothesis 1 predicts a tie on capture with ZFS fast dedup. The curve beneath the tie, capture against index memory across three chunk sizes, is this page's result.
</p>

<h2>Configurations</h2>
<p>
	<strong>R0. Raw file on XFS.</strong><br />
	QEMU's raw driver on the dedicated NVMe.<br />
	The control, with no deduplication anywhere in the path.
</p>
<p>
	<strong>R1. Zvol on pinned OpenZFS fast dedup.</strong><br />
	Its own pool on the same device, created and destroyed per run, opened by QEMU as a block device. The current Nixpkgs lock selects <a href="https://github.com/openzfs/zfs/releases/tag/zfs-2.4.4" target="_blank" rel="noopener">OpenZFS 2.4.4</a>. Freeze the release, host kernel and pool settings for each measurement cohort; a version change reruns the affected controls.
</p>
<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Setting</th><th>Value</th><th>Why</th></tr>
		</thead>
		<tbody>
			<tr><td class="k"><code>feature@fast_dedup</code></td><td>enabled</td><td>DDT update log</td></tr>
			<tr><td class="k"><code>dedup</code></td><td><code>blake3</code></td><td><a href="https://github.com/openzfs/zfs/blob/zfs-2.4.4/man/man7/zfsprops.7" target="_blank" rel="noopener">SHA-256 with <code>dedup=on</code></a></td></tr>
			<tr><td class="k"><code>volblocksize</code></td><td><code>16K</code> primary arm, <code>4K</code> second arm</td><td>the zvol's dedup granularity</td></tr>
			<tr><td class="k"><code>compression</code></td><td><code>zle</code></td><td>all-zero blocks become holes</td></tr>
			<tr><td class="k"><code>dedup_table_quota</code></td><td><code>none</code></td><td>uncapped DDT</td></tr>
			<tr><td class="k"><code>zpool ddtprune</code></td><td>never run during a measurement</td><td>no entries dropped</td></tr>
			<tr><td class="k"><code>primarycache</code></td><td><code>all</code></td><td>explicit ARC policy</td></tr>
			<tr><td class="k">DDT memory</td><td><code>zpool status -DD</code>, <code>dedupcached</code></td><td>resident DDT bytes</td></tr>
		</tbody>
	</table>
</div>
<p>
	OpenZFS 2.4.4 does not support its ARC-bypassing direct-IO path for zvols, so R1 remains ARC-backed. Filesystem direct writes instead skip deduplication; replacing the zvol with a direct-IO file would change the comparator. <a href="https://github.com/openzfs/zfs/blob/zfs-2.4.4/man/man7/zfsprops.7" target="_blank" rel="noopener">OpenZFS properties</a>
</p>
<p>
	The R1 profile verifies nonzero deduplication and zero handling before measurement. Record pool features, quota, sync settings, ARC residency and total host memory alongside the DDT measures. Report <code>dedup_table_size</code> as disk footprint and zero-run compression savings separately. <a href="https://github.com/openzfs/zfs/blob/zfs-2.4.4/man/man7/zpoolprops.7" target="_blank" rel="noopener">Pool properties</a>, <a href="https://github.com/openzfs/zfs/blob/zfs-2.4.4/man/man8/zpool-status.8" target="_blank" rel="noopener">DDT statistics</a>
</p>
<p>
	<strong>R2. Raw file on XFS over dm-vdo</strong> (optional).<br />
	Fixed 4 KiB deduplication in the kernel, with its own XFS instance on the vdo device.<br />
	Index memory from <code>vdostats</code>.
</p>
<p>
	<strong>R3. The backend on one host.</strong><br />
	Local store only, so k does not apply.<br />
	Three chunk-size arms, below.
</p>
<p>
	R0 against R3 is the cost of the daemon with everything else held constant.<br />
	R1 is the deployed comparator and differs in kernel boundary, caching, and allocation, so it is a case study beside the controlled pair, and deltas are attributed accordingly.
</p>

<h2>Chunk-size arms</h2>
<p>
	Fixed 4 KiB chunks cost one index entry per 4 KiB:
</p>
<ul class="plain">
	<li>about 250 million entries per TB</li>
	<li>about 10 GB of memory per TB at 40 bytes per entry, a 32-byte hash and an 8-byte offset</li>
</ul>
<p>
	The alignment argument on page 00 predicts that they capture nearly every duplicate a Linux guest holds. The census measures the remainder.<br />
	FastCDC with a 16 KiB mean cuts the index by four and loses an aligned 4 KiB match whenever the rest of its chunk differs.<br />
	The one prior curve on VM images is Liquid's: 77% of bytes removed at 4 KiB, falling to 59% at 256 KiB on 183 images, with 256 KiB chosen for HDD seek cost. On NVMe the seek term is gone and the trade is index memory against capture.
</p>
<p>
	Three arms: fixed 4 KiB, fixed 16 KiB, FastCDC 8 to 64 KiB with a 16 KiB mean.<br />
	CDC boundaries snap to 4 KiB, so no guest block straddles two chunks and a 4 KiB overwrite invalidates one chunk, not two.<br />
	Reported per arm: bytes stored, index bytes per TB, guest p99, write amplification, compactor CPU per GB.<br />
	The census below predicts the capture column for each arm before any run.
</p>

<h2>Workloads</h2>
<ul class="plain">
	<li>fio: 4 KiB random write and read at QD1 and QD32; 128 KiB sequential.</li>
	<li>Boot storm: n clones of one image booted together, n = 4, 16, 32. A clone is a copy of the manifest with its own staging log.</li>
	<li>Fleet replay: the synthetic fleet below written onto n guests, at two points on its timeline.</li>
	<li>Overwrite: a small SQLite database rewriting its pages in place for an hour, with guest discard on.</li>
	<li>Pressure: sustained unique writes and overwrite bursts beside a reading guest; run through admission pressure and idle drain. Repeat reads with shared and disjoint working sets and a scanning neighbor.</li>
</ul>

<h2>Metrics</h2>
<ul class="plain">
	<li>Guest p50 and p99 write and read latency against R0, compactor active and idle. Reported first.</li>
	<li>Bytes stored after compaction completes and the sweep has run, against the census prediction at the configuration's chunk or block size. Bytes the sweep reclaimed reported beside it as the leak.</li>
	<li>Index or DDT bytes per stored TB.</li>
	<li>Write amplification: device bytes written per guest block-device byte, from NVMe counters. Record application bytes, virtio payload, staging records and fences, chunk writes, and metadata/GC traffic separately. Guest filesystem journaling can make application bytes differ from block-device bytes.</li>
	<li>Sustainable ingest, staging allocations, live and dead bytes, and compaction progress. Report the point where admission slows, each guest's latency, and idle drain time.</li>
	<li>Chunk traffic against the settle window: chunks produced per guest byte written, on the overwrite workload.</li>
	<li>Compactor CPU per GB ingested, per chunk-size arm.</li>
	<li>Host payload bytes copied, peak append/fetch-buffer bytes, cache occupancy and total resident memory, with shared mappings counted once.</li>
	<li>Recovery: the page 01 tests pass before any number is reported.</li>
</ul>

<h2>Controls</h2>
<p>
	Pinned vCPUs, performance governor, discarded warm-up, fresh filesystem or pool per repetition, at least five repetitions, variance beside every number.<br />
	With <code>cache=none</code>, R0 and R2 bypass the host file-data cache. R1 retains the ARC. Choose <code>zfs_arc_max</code> and the R3 clean-cache limit within the equal total memory budget below. Report actual ARC data/metadata residency, DDT residency, ZFS dirty memory and daemon buffers/indexes separately; equal cache caps do not establish equal total memory use.
</p>
<p>
	All configurations are observed at the guest boundary (fio's histograms, guest-side blktrace for the boot storm) plus host device counters.<br />
	The daemon adds per-request stage timestamps drained to ndjson, cross-checked once against bpftrace with the delta reported.<br />
	<code>zpool</code> and <code>vdostats</code> figures are supplementary.
</p>

<p>
	Guest filesystem workloads retain normal caching. Direct guest fio runs isolate block-device costs and are labeled separately. Host IO mode, guest IO mode and FLUSH frequency are recorded independently.<br />
	The host buffered/direct staging comparison holds format, queue depth, durability and total memory budget constant. Record packing and submission concurrency are varied separately. A one-block write followed by FLUSH exposes fence overhead.
</p>
<p>
	Memory comparisons use equal total host budgets, including guest RAM, daemon buffers, indexes, caches and kernel file-data cache. Shared mappings are counted once. Slowed-compactor and owner-outage runs test the reserve and failure paths from page 01.
</p>

<h2>Guest memory sharing <span class="tag-stretch">proposed</span></h2>
<p>
	A separate experiment would compare the same immutable image through virtio-blk and virtio-pmem/DAX, with identical private OverlayFS uppers. First verify DAX use, cross-guest isolation, and recovery of synchronized upper-layer writes. Then measure aggregate resident memory without double-counting shared pages, guest memory use, read latency, CPU, and copy-up bytes as guest count grows. Hold contents, memory budgets, and workloads constant; separate cold host cache, warm host cache with cold guest cache, and repeated accesses.
</p>
<p>
	This changes the guest storage stack, so it has its own results table alongside R0–R3. Its first question is whether mapped reads reduce memory duplication for shared files. No performance threshold is set; further CAS integration depends on the feasibility result.
</p>

<h2>Payload placement <span class="tag-stretch">proposed</span></h2>
<p>
	The baseline copies surviving staging data into a separate chunk store. An alternative at fixed 4 KiB would hash surviving records and publish their existing payload locations. Segment cleaning could still move live data to reclaim space. Larger chunks assembled from scattered writes may require copying.
</p>
<p>
	Both layouts must recover acknowledged FLUSHes before comparison. Hold workload, durability, memory and available disk space constant. Measure unique writes, overwrites and deletion at increasing disk occupancy; report all payload writes, retained segment bytes, cleaning traffic and guest latency. This experiment leaves the guest contract unchanged.
</p>

<h2>Census</h2>
<p>
	A small census supplies the numbers the rest of the study is measured against: how many unique bytes the fleet holds under each arm's chunker, and how many bytes copy-on-write would already have shared.
</p>
<p>
	<strong>Phase 0.</strong><br />
	<code>zdb -S</code> on a ZFS pool holding the cloned fleet.<br />
	Pool traversal starts each dataset at its origin snapshot's transaction group, so blocks a clone inherited are counted once and the simulated ratio is duplicates beyond what clones already share. This reading of <code>dmu_traverse.c</code> is confirmed with a two-clone test before the number is cited.
</p>
<p>
	<strong>The fleet.</strong><br />
	Ubuntu publishes dated cloud images and a <a href="https://snapshot.ubuntu.com/">dated package archive</a>.
	The controlled Ubuntu fleet uses that archive; Debian's snapshot.debian.org is an alternative for Debian guests.<br />
	An image installed as of T0 and upgraded monthly against the archive as of T1, T2, and on replays a real update history.<br />
	n such clones with scripted drift (hostnames, logs, a few packages each) form the fleet.<br />
	It is rebuilt by one command, dated, and is also the replay workload above.
</p>
<p>
	<strong>The split.</strong><br />
	Per byte range: zero or unallocated (from the guest allocation map, excluded), unique, shared with the T0 base in place, duplicate at an aligned 4 KiB or 16 KiB boundary elsewhere in the fleet, or duplicate only at a shifted offset.<br />
	The aligned columns predict R1 and the fixed arms.<br />
	The CDC arm is predicted by running FastCDC with the arm's parameters over the images, because a 16 KiB mean chunk captures fewer aligned matches than fixed 4 KiB chunks and more shifted ones, and the two effects do not add.<br />
	There are no donors, no real fleets, and no claims about time.
</p>

<PageNav num="02" />
