<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="02" />
<p class="lede">
	<strong>Part 1.</strong><br />
	Same stock QEMU, same guest, same NVMe device, storage behind the device varies.<br />
	The prediction is a tie on capture between the daemon and ZFS fast dedup.<br />
	It is measured anyway, because the chunk-size curve under it is the single-host design result, and because a reviewer will ask why not ZFS.
</p>

<h2>Rungs</h2>
<ul class="reqs">
	<li>
		<span class="rid">R0</span><strong>Raw file on XFS.</strong><br />
		QEMU's raw driver on the dedicated NVMe.<br />
		The control; no dedup anywhere in the path.
	</li>
	<li>
		<span class="rid">R1</span><strong>Zvol on ZFS 2.3 fast dedup.</strong><br />
		Own pool on the same device, created and destroyed per run, opened by QEMU as a block device.<br />
		<code>feature@fast_dedup</code>; <code>dedup=blake3</code>, since <code>dedup=on</code> silently uses SHA-256 regardless of the checksum property; <code>volblocksize=16K</code> primary and <code>4K</code> second arm; <code>compression=zle</code> outside the compression arm so zero blocks do not collapse onto one DDT entry; <code>dedup_table_quota</code> unset and <code>zpool ddtprune</code> never run during a measurement; DDT memory from <code>zpool status -D</code>.<br />
		OpenZFS direct IO does not apply to zvols or with dedup, so R1 is ARC-backed in every arm and the paper says so.
	</li>
	<li>
		<span class="rid">R2</span><strong>Raw file on XFS over dm-vdo.</strong><span class="tag-stretch">optional</span><br />
		Inline fixed-4K dedup in the kernel, mainline since 6.9.<br />
		Its own XFS instance on the vdo device.<br />
		Index memory from <code>vdostats</code>.
	</li>
	<li>
		<span class="rid">R4</span><strong>The daemon, one host.</strong><br />
		Local store only, k not in play.<br />
		Three chunk-size arms below.
	</li>
</ul>
<p>
	R0 against R4 is the cost of the daemon with everything else held constant.<br />
	R1 is the deployed state of the art and differs in kernel boundary, caching, and allocation, so it is a case study beside the controlled pair, and the paper attributes deltas accordingly.
</p>

<h2>Chunk size</h2>
<p>
	Fixed 4K captures everything a Linux guest offers, and costs an index entry per 4K: about 250 million entries per TB, roughly 10 GB of RAM per TB at 40 bytes each.<br />
	That is the DDT memory problem the daemon is supposed to escape.<br />
	FastCDC at a 16K mean cuts the index four times over and loses some aligned matches.
</p>
<p>
	Three arms: fixed 4K, fixed 16K, FastCDC 8K to 64K with a 16K mean.<br />
	Reported per arm: bytes stored, index bytes per TB, guest p99, write amplification.<br />
	<mark>Capture against index memory as a function of chunk size is the curve this page exists for.</mark><br />
	The census below predicts the capture column before any run.
</p>

<h2>Workloads</h2>
<ul class="plain">
	<li>fio: 4K random write and read at QD1 and QD32; 128K sequential.</li>
	<li>Boot storm: N clones of one image booted together, N = 4, 16, 32.</li>
	<li>Fleet replay: the synthetic fleet below written onto N guests, at two points on its timeline.</li>
</ul>
<p>
	No kernel build and no synthetic stress workload that exists only to exercise the daemon.
</p>

<h2>Metrics</h2>
<ul class="plain">
	<li>Guest p50 and p99 write and read latency against R0, compactor active and idle. Reported first.</li>
	<li>Bytes stored after compaction settles, against the census prediction at the rung's block size.</li>
	<li>Index or DDT bytes per stored TB.</li>
	<li>Write amplification: device bytes written per guest byte, from NVMe counters.</li>
	<li>Sustainable ingest and the back-pressure point.</li>
	<li>Recovery: <code>kill -9</code>, replay, <code>fio --verify</code>.</li>
</ul>

<h2>Controls</h2>
<p>
	Pinned vCPUs, performance governor, discarded warm-up, fresh filesystem or pool per repetition, at least five repetitions, variance beside every number.<br />
	Cache bounded equal across rungs: cgroup memory limit for the page cache on R0 and R2, <code>zfs_arc_max</code> on R1, the daemon's cache size on R4.
</p>
<p>
	All rungs are observed at the guest boundary (fio's histograms, guest-side blktrace for the boot storm) plus host device counters.<br />
	The daemon adds per-request stage timestamps drained to ndjson, cross-checked once against bpftrace with the delta reported.<br />
	<code>zpool</code> and <code>vdostats</code> figures are supplementary.
</p>

<h2>Prediction</h2>
<p>
	A small census supplies two numbers the rest of the study is measured against: how many unique bytes a fleet holds at a given block size, and how many bytes copy-on-write would already have shared.
</p>
<p>
	<strong>Phase 0.</strong><br />
	<code>zdb -S</code> on a ZFS pool holding the cloned fleet.<br />
	Pool traversal starts each dataset at its previous snapshot's txg, so blocks a clone inherited from its origin are counted once, and the simulated ratio is duplicates beyond what clones already share.<br />
	Verified in <code>dmu_traverse.c</code>; confirmed by a five-minute test before it is cited.
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
	Per byte range: zero or unallocated (from the guest allocation map, excluded), unique, shared with the T0 base in place, duplicate at an aligned 4K or 16K boundary elsewhere in the fleet, or duplicate only at a shifted offset.<br />
	The aligned column predicts R1 and the fixed arms; aligned plus shifted predicts the CDC arm.<br />
	Nothing more: no donors, no real fleets, no time-axis claims.
</p>

<PageNav num="02" />
