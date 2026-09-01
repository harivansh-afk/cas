<script lang="ts">
	import { base } from '$app/paths';
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="02" />
<p class="lede">
	<mark>Zero new code.</mark>
	Same stock QEMU, same guest, same NVMe device, four backends an operator can turn on today.
	This is what makes the cost table a decision rule rather than a curiosity.
</p>

<h2>Why stock systems</h2>
<p>
	The census's middle tier, block-capturable bytes, is exactly what shipping systems reach: an
	inline dedup table at aligned granularity, or a post-process pass over reflinks.
	Pricing that tier on the systems people actually run answers the operator's question directly.
	A research daemon would price a design nobody deploys, and its constants would generalize only
	by argument.
	Constants for stock systems generalize by being the thing itself.
</p>

<h2>The rungs</h2>
<p>
	Same stock QEMU configuration and cache mode (A4) for all four; only the storage behind the
	virtio-blk device varies.
	R0, R2, and R3 share one XFS filesystem on the dedicated NVMe; R1 has its own pool on the same
	device, created and destroyed per run.
</p>
<ul class="reqs">
	<li>
		<span class="rid">R0</span><strong>Raw file on XFS.</strong> The control, through QEMU's
		built-in raw driver. Dedup nowhere in the path.
	</li>
	<li>
		<span class="rid">R1</span><strong>Raw file on a ZFS zvol, stock OpenZFS ≥ 2.3 with fast
		dedup.</strong>
		The incumbent block-pointer design with content-hash identity.
		Fast dedup is required, not optional: it replaces the legacy DDT's random writes with a
		sorted log flush and adds a quota and pruning, and a comparison against the pre-2.3 DDT would
		be against a design its own maintainers call obsolete.
		Configuration: <code>feature@fast_dedup</code> enabled; <code>checksum=blake3, dedup=on</code>;
		<code>volblocksize</code> arms at 4K and 16K, since zvol dedup granularity is the volblocksize
		and the census's fixed-block result decides which is headline; <code>dedup_table_quota</code>
		unset and <code>zpool ddtprune</code> never run during a measurement, both recorded;
		compression off outside the labeled compression arm; DDT memory read from
		<code>zpool status -D</code>.
		Whether OpenZFS direct IO applies to a zvol with dedup on is checked and recorded before the
		media-honest run.
	</li>
	<li>
		<span class="rid">R2</span><strong>Raw file on XFS over dm-vdo.</strong>
		Inline fixed-4K dedup and compression in the kernel, mainline since 6.9.
		Configuration: dedup on, compression off outside the compression arm, index memory from
		<code>vdostats</code>.
		The cleanest controlled comparison in the set: identical filesystem and file to R0, one
		device-mapper layer added.
	</li>
	<li>
		<span class="rid">R3</span><strong>Raw file on XFS with post-process
		<code>duperemove</code>.</strong>
		Fixed-block dedup by <code>FIDEDUPERANGE</code> at a declared cadence, on the control
		filesystem itself.
		The zero-cost-on-the-write-path point: no inline hashing, no table on the hot path, capture
		deferred to a batch job.
		Storage and transfer are read after the pass settles; the pass's own device traffic is
		reported as its write amplification.
	</li>
</ul>
<p>
	R0 versus R2 prices inline aligned content identity with everything else held constant.
	R0 versus R3 prices post-process aligned content identity the same way.
	R1 anchors both against the deployed state of the art and, read against the census at the
	matching volblocksize, shows how much of the cross-lineage segment a dedup table reaches in
	practice against what the census says it could.
	R1 is a case study rather than a controlled comparison: it differs from the others in kernel
	boundary, caching, and allocation, and the paper attributes cross-rung deltas accordingly.
</p>

<h2>Workloads</h2>
<p>
	fio, 4K random write and read at QD1 and QD32, 128K sequential; kernel build in the guest;
	N-clone boot storm, N at 4, 16, and 32; replay of one real fleet from the census onto N guests,
	which is the workload the whole study is about.
	Synthetic stress workloads that exist only to exercise a daemon are not in this phase.
</p>

<h2>Measured per rung</h2>
<p>
	Two headline metrics and their preconditions.
	<mark>Transfer</mark>: bytes that had to move to provision the N clones and to replay the fleet,
	from device write counters, against the census's unique-byte count for the same images.
	<mark>Cache</mark>: host device reads per guest byte read during the boot storm, so N guests
	reading one shared block show up as one read.
	Preconditions: guest p50 and p99 write and read latency against R0, at least five repetitions,
	variance beside every number; write amplification, device bytes written per guest byte, from
	NVMe counters; storage consumed after ingest and after any post-process pass settles; index or
	DDT memory per stored TB.
</p>
<p>
	Latency parity is a precondition and reported first.
	A backend that captures everything and doubles p99 has a different cost than one that captures
	everything at parity, and the table says which is which before it says how much was captured.
</p>

<h2>Instrumentation and controls</h2>
<p>
	All rungs are observed at the guest boundary (fio's own latency histograms, guest-side
	<code>blktrace</code> for the build and boot storm) plus host device counters, so no rung is
	favored by internal tracing the others lack.
	ZFS adds <code>zpool</code> statistics and dm-vdo adds <code>vdostats</code>, reported as
	supplementary.
	Controls: pinned vCPUs, performance governor, discarded warm-up, fresh filesystem or pool per
	repetition, at least five repetitions.
	Gate G3 is a complete table: four rungs, identical workloads, no empty cells.
</p>

<h2>What this phase cannot say</h2>
<p>
	Nothing here reaches the <a class="term" href="{base}/00#term-cdc-only">CDC-only</a> leaf.
	Every backend in this phase is aligned, so the table prices two of the three tiers.
	If the census says the third tier is small on VM images, the table is the whole cost side and
	the study is complete.
	If it says the third tier is large, page 03 builds the instrument that prices it.
</p>

<PageNav num="02" />
