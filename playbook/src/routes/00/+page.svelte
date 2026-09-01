<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="00" />
<p class="lede">
	<strong>Goal of the study.</strong> Measure whether chunk-level content addressing finds enough
	duplicate data that copy-on-write systems structurally miss to justify its runtime costs. The
	instrument is a purpose-built two-tier storage backend; the baseline is stock ZFS; the evidence
	is a redundancy census over real corpora and a four-rung system comparison.
</p>

<h2>The structural gap</h2>
<p>
	Two VMs each run <code>apt upgrade</code> and download the same packages. Their disks now hold
	identical bytes. No snapshot, clone, or backing chain can share those bytes, because neither copy
	descends from the other. Copy-on-write shares data that was copied. It cannot share data that
	became equal. This study calls the difference cross-lineage redundancy. A content-addressed store
	captures it because identity is the address; a block-pointer store cannot, regardless of tuning.
</p>
<p>
	The size of that gap on real data is unmeasured. The community's operating rule (despairlabs,
	2024) is that dedup pays only when clients cannot or will not issue an explicit copy signal; no
	measurement of how much sharing the signal misses exists. Granularity studies exist for desktops
	(Meyer &amp; Bolosky, FAST '11), containers (DupHunter, ATC '20), and models (ZipLLM 2025; Xet
	production data). None measures the lineage axis. VM fleet data dates to 2009 (Jin &amp; Miller,
	SYSTOR '09).
</p>

<h2>The objection this study must survive</h2>
<p>
	Raw capacity is cheap; NVMe retails on the order of $50–100 per TB. Capturing even half of a
	small fleet's bytes saves little money at rest. The claim is therefore not "disks get smaller."
	Captured redundancy is worth measuring because it prices three things at once: capacity
	(compounding at fleet scale), transfer (sync, migration, and provisioning move unique bytes
	only), and cache (N guests reading one shared chunk occupy one page-cache entry, not N). The
	census and the system comparison report all three separately. If all three come back small, the
	study reports that copy-on-write plus zstd is sufficient, with the numbers to show it.
</p>

<h2>Hypotheses</h2>
<ul class="reqs">
	<li>
		<span class="rid">H1</span>In multi-VM fleets, a substantial fraction of duplicate bytes lies
		across lineage boundaries. Measured offline on five corpus classes. Falsifiable; a small result
		reverses the recommendation and still stands as a result.
	</li>
	<li>
		<span class="rid">H2</span>A two-tier backend, a durable staging log ahead of a
		content-addressing compactor, captures cross-lineage redundancy with guest-visible write
		latency comparable to a raw-file backend. The costs relocate to write amplification, compaction
		bandwidth, and index memory; all three are measured.
	</li>
	<li>
		<span class="rid">H3</span>Chunk pointers distribute where block pointers do not: a chunk's
		placement is a function of its name, so the capacity tier spreads across hosts without shared
		allocation state. Argued from the design; demonstrated on two nodes; not benchmarked further.
	</li>
</ul>

<h2>Hardware</h2>
<p>
	The study runs on x86-64 bare metal. This is the architecture of every system in the comparison
	literature (Meyer &amp; Bolosky, DupHunter, the ZFS deployment base), so results compare directly
	to prior work.
</p>
<p>
	Primary testbed: two CloudLab c6525-100g nodes (Utah cluster). Per node: one AMD EPYC 7402P, 24
	cores at 2.80 GHz, Zen 2; 128 GB ECC DDR4-3200 (8 × 16 GB RDIMM); two 1.6 TB NVMe SSDs, PCIe
	4.0; one 25 GbE and one 100 GbE experiment link. One NVMe device holds the system and results;
	the second is dedicated to the store under test, so guest IO and compaction never share a device
	with the OS. The 100 GbE pair carries S3. CloudLab allocations are free for sponsored academic
	research; the sponsor approves the project.
</p>
<p>
	Fallback if CloudLab access is not granted: two OVHcloud Advance bare-metal servers (2026 line):
	AMD EPYC 4005-series, 16 cores/32 threads, DDR5 ECC, 2 × 960 GB NVMe, 25 Gbps private bandwidth.
</p>
<p>
	No RDMA, no persistent memory, no accelerators on either testbed; the commodity restriction is
	part of the claim. Every throughput and latency figure in the paper is measured on the testbed.
	None is quoted from vendors or prior work.
</p>

<h2>Assumptions</h2>
<ul class="reqs">
	<li>
		<span class="rid">A1</span>Workload class: hosts serving multiple guests from local flash,
		homelab to rack scale. Array economics out of scope.
	</li>
	<li>
		<span class="rid">A2</span>Experiments run at single-digit TB. Index, amplification, and
		compaction costs are reported as formulas with measured constants; the 100 TB figures are
		labeled extrapolations.
	</li>
	<li>
		<span class="rid">A3</span>Equal BLAKE3 (256-bit) implies equal bytes. A verify-on-dedup arm
		bounds the risk empirically (Henson, HotOS '03, cited).
	</li>
	<li>
		<span class="rid">A4</span>The guest contract is virtio-blk with a volatile write cache: an
		acknowledged FLUSH is durable, nothing else is.
	</li>
	<li>
		<span class="rid">A5</span>One image, one writer. Shared-disk clustering out of scope.
	</li>
	<li>
		<span class="rid">A6</span>Dedup side channels and convergent-encryption probing are documented
		and excluded; the store is trusted infrastructure here.
	</li>
	<li>
		<span class="rid">A7</span>Compression is zstd, measured in both orders relative to dedup.
		All-zero and unallocated ranges are excluded from every ratio and reported separately.
	</li>
	<li>
		<span class="rid">A8</span>Corpora represent their declared classes only. Build scripts are
		published; results are per class; no universal ratio is claimed.
	</li>
</ul>

<PageNav num="00" />
