<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="02" />
<p class="lede">
	Part 1 runs the same stock QEMU, the same guest, and the same NVMe device, and varies only the storage behind the device.<br />
	We predict a tie on capture between the backend and ZFS fast dedup.<br />
	The measurement is made because the chunk-size curve beneath the tie is the single-host design result, and because the tie is hypothesis 1.
</p>

<h2>Configurations</h2>
<p>
	<strong>R0. Raw file on XFS.</strong><br />
	QEMU's raw driver on the dedicated NVMe.<br />
	The control, with no deduplication anywhere in the path.
</p>
<p>
	<strong>R1. Zvol on ZFS 2.3 fast dedup.</strong><br />
	Its own pool on the same device, created and destroyed per run, opened by QEMU as a block device.
</p>
<div class="table-scroll">
	<table class="spec prose">
		<thead>
			<tr><th>Setting</th><th>Value</th><th>Why</th></tr>
		</thead>
		<tbody>
			<tr><td class="k"><code>feature@fast_dedup</code></td><td>enabled</td><td>the 2.3 dedup table with its log</td></tr>
			<tr><td class="k"><code>dedup</code></td><td><code>blake3</code></td><td><code>dedup=on</code> hashes with SHA-256 regardless of the <a href="https://openzfs.github.io/openzfs-docs/man/master/7/zfsprops.7.html" target="_blank" rel="noopener">checksum property</a></td></tr>
			<tr><td class="k"><code>volblocksize</code></td><td><code>16K</code> primary arm, <code>4K</code> second arm</td><td>the zvol's dedup granularity</td></tr>
			<tr><td class="k"><code>compression</code></td><td><code>zle</code></td><td>zero blocks would otherwise collapse onto one DDT entry</td></tr>
			<tr><td class="k"><code>dedup_table_quota</code></td><td>unset</td><td>no cap on the table during a measurement</td></tr>
			<tr><td class="k"><code>zpool ddtprune</code></td><td>never run during a measurement</td><td>no entries dropped</td></tr>
			<tr><td class="k">DDT memory</td><td><code>zpool status -D</code></td><td>the index-memory column</td></tr>
		</tbody>
	</table>
</div>
<p>
	OpenZFS direct IO applies neither to zvols nor with deduplication enabled, so R1 is ARC-backed in every arm and is reported as such.
</p>
<p>
	<strong>R2. Raw file on XFS over dm-vdo</strong> (optional).<br />
	Inline fixed-4 KiB deduplication in the kernel, mainline since 6.9, with its own XFS instance on the vdo device.<br />
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
	<mark>Capture against index memory as a function of chunk size is the result this page produces.</mark><br />
	The census below predicts the capture column for each arm before any run.
</p>

<h2>Testbench workloads</h2>
<ul class="plain">
	<li>fio: 4 KiB random write and read at QD1 and QD32; 128 KiB sequential.</li>
	<li>Boot storm: N clones of one image booted together, N = 4, 16, 32. A clone is a copy of the manifest with its own staging log.</li>
	<li>Fleet replay: the synthetic fleet below written onto N guests, at two points on its timeline.</li>
	<li>Overwrite: a small SQLite database rewriting its pages in place for an hour, with guest discard on. This is the case where a store without reference counts leaks between sweeps and ZFS does not.</li>
</ul>

<h2>Metrics</h2>
<ul class="plain">
	<li>Guest p50 and p99 write and read latency against R0, compactor active and idle. Reported first.</li>
	<li>Bytes stored after compaction completes and the sweep has run, against the census prediction at the configuration's block size. Bytes the sweep reclaimed reported beside it as the leak.</li>
	<li>Index or DDT bytes per stored TB.</li>
	<li>Write amplification: device bytes written per guest byte, from NVMe counters, with both legs (staging and store) reported, not one.</li>
	<li>Sustainable ingest, the point where the governor starts adding latency, and how much it adds.</li>
	<li>Chunk traffic against the settle window: chunks produced per guest byte written, on the overwrite workload.</li>
	<li>Compactor CPU per GB ingested, per chunk-size arm. Liquid gave hashing cost as its reason for large blocks, and here it is a number.</li>
	<li>Recovery: <code>kill -9</code>, replay, <code>fio --verify</code>; FLUSH covering writes on another queue; an empty discard; a daemon that stops answering.</li>
</ul>

<h2>Controls</h2>
<p>
	Pinned vCPUs, performance governor, discarded warm-up, fresh filesystem or pool per repetition, at least five repetitions, variance beside every number.<br />
	With <code>cache=none</code>, R0 and R2 have no host cache. <code>zfs_arc_max</code> on R1 and the daemon's cache size on R3 are set equal.<br />
	The ARC also holds the DDT, so R1's data cache is smaller than R3's by the DDT's size. The DDT size is reported beside the cache size so the difference is visible.
</p>
<p>
	All configurations are observed at the guest boundary (fio's histograms, guest-side blktrace for the boot storm) plus host device counters.<br />
	The daemon adds per-request stage timestamps drained to ndjson, cross-checked once against bpftrace with the delta reported.<br />
	<code>zpool</code> and <code>vdostats</code> figures are supplementary.
</p>

<h2>Prediction from a small census</h2>
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
	Ubuntu publishes dated cloud images and snapshot.debian.org serves the archive as of any date.<br />
	An image installed as of T0 and upgraded monthly against the archive as of T1, T2, and on replays a real update history.<br />
	N such clones with scripted drift (hostnames, logs, a few packages each) form the fleet.<br />
	It is rebuilt by one command, dated, and is also the replay workload above.
</p>
<p>
	<strong>The split.</strong><br />
	Per byte range: zero or unallocated (from the guest allocation map, excluded), unique, shared with the T0 base in place, duplicate at an aligned 4 KiB or 16 KiB boundary elsewhere in the fleet, or duplicate only at a shifted offset.<br />
	The aligned columns predict R1 and the fixed arms.<br />
	The CDC arm is predicted by running FastCDC with the arm's parameters over the images, because a 16 KiB mean chunk captures fewer aligned matches than 4 KiB blocks and more shifted ones, and the two effects do not add.<br />
	Nothing further: no donors, no real fleets, no claims about time.
</p>

<PageNav num="02" />
