<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="00" />
<p class="lede">
	<strong>Goal.</strong> Measure whether chunk-level content addressing finds enough duplicate
	data that copy-on-write systems structurally miss to justify its runtime costs.
	The instrument is a two-tier storage backend.
	The baseline is stock ZFS.
	The evidence is a redundancy census over real corpora and a four-rung system comparison.
</p>

<h2>The structural gap</h2>
<p>
	Two VMs each run <code>apt upgrade</code> and download the same packages.
	Their disks now hold identical bytes, and no snapshot, clone, or backing chain can share them,
	because neither copy descends from the other.
</p>
<p>
	Copy-on-write shares data that was copied.
	It cannot share data that became equal.
	This study calls the difference cross-lineage redundancy.
	A content-addressed store captures it because the address is the content; a block-pointer store
	cannot, regardless of tuning.
</p>

<h2>Prior art</h2>
<p>
	Every published dedup study answers one question: how much duplicate data exists.
	This study answers a different one: how much duplicate data requires content addressing to
	capture, given that clones and reflinks already capture the history-shaped part for free.
	The second question is the one that decides deployment, and no study on record asks it.
</p>

<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Prior work</th><th>What it measured</th><th>What it cannot answer</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">Meyer &amp; Bolosky, FAST '11</td><td>file- vs block-level dedup ratios on 857 desktops</td><td>no lineage axis; no VM corpora</td></tr>
			<tr><td class="k">Jin &amp; Miller, SYSTOR '09</td><td>dedup ratios across VM disk images</td><td>ratios only; no COW baseline; 2009 corpora</td></tr>
			<tr><td class="k">DupHunter, ATC '20</td><td>file-level redundancy across Docker Hub</td><td>no block granularity; no lineage axis</td></tr>
			<tr><td class="k">iDedup FAST '12 · Dmdedup · VDO</td><td>cost of inline block dedup on primary storage</td><td>never compared against what COW capture gets free</td></tr>
			<tr><td class="k">OpenZFS</td><td>ships clones and dedup side by side since 2009</td><td>no published split of their respective capture</td></tr>
			<tr><td class="k">CLB, VEE '17</td><td>content addressing as a VM read-cache optimization</td><td>uses content addressing; never prices it</td></tr>
			<tr><td class="k">casync · restic · borg</td><td>chunk-level CAS deployed at scale, for backup</td><td>no latency contract; no peer-reviewed measurement</td></tr>
			<tr><td class="k">VAST · Pure</td><td>commercial data-reduction ratios</td><td>proprietary, undecomposed, unreproducible</td></tr>
		</tbody>
	</table>
</div>

<p>
	The gap exists because the two mechanisms belong to different communities.
	Filesystem developers ship clones and stopped there; backup tools ship chunking and never had a
	COW baseline to subtract.
	The split between what history can share and what only content can share has never been
	measured, on any corpus, by anyone.
	The census produces that split; the rungs price capturing it.
</p>

<h2>The objection this study must survive</h2>
<p>
	Raw capacity is cheap.
	NVMe retails on the order of $50–100 per TB, so halving a small fleet's bytes saves little
	money at rest.
</p>
<p>
	The claim is therefore not that disks get smaller.
	Captured redundancy prices three things: capacity, which compounds at fleet scale; transfer,
	since sync, migration, and provisioning move unique bytes only; and cache, since N guests
	reading a shared chunk occupy one page-cache entry rather than N.
	The census and the comparison report the three separately.
	If all three come back small, the study reports that copy-on-write plus zstd is sufficient,
	with the numbers to show it.
</p>

<h2>What the study proves</h2>
<p>
	If H1 holds and H2's costs are acceptable, the study replaces a folklore decision with a
	measured rule: deploy content addressing for a workload class when its cross-lineage fraction,
	priced across capacity, transfer, and cache, exceeds the measured taxes of hashing, indexing,
	and compaction.
	Operators currently make this call from forum anecdotes, and the ZFS community's own guidance
	reduces to "probably not, for most people."
</p>
<p>
	If H1 fails, the result is equally usable: for the measured classes, clones plus zstd capture
	nearly everything, and building block-level dedup for such fleets is wasted engineering.
	Either verdict changes what someone should build, which is what makes either verdict
	publishable.
</p>
<p>
	Three artifacts outlive the verdict.
	The census pipeline and corpus scripts run on anyone's fleet, so the measurement repeats where
	the folklore cannot.
	The VM redundancy numbers are the first since 2009.
	The compactor constants — sustained-ingest ceiling, write amplification, interference — are the
	first published for a content-addressing compactor.
</p>

<h2>Hypotheses</h2>
<ul class="reqs">
	<li>
		<span class="rid">H1</span>In multi-VM fleets, a substantial fraction of duplicate bytes lies
		across lineage boundaries.
		Measured offline on five corpus classes.
		A small result reverses the recommendation and still stands as a result.
	</li>
	<li>
		<span class="rid">H2</span>A two-tier backend, a durable staging log ahead of a
		content-addressing compactor, captures cross-lineage redundancy with guest-visible write
		latency comparable to a raw-file backend.
		The costs relocate to write amplification, compaction bandwidth, and index memory; all three
		are measured.
	</li>
	<li>
		<span class="rid">H3</span>Chunk pointers distribute where block pointers do not: a chunk's
		placement is a function of its name, so the capacity tier spreads across hosts without shared
		allocation state.
		Argued from the design; demonstrated on two nodes; not benchmarked further.
	</li>
</ul>

<h2>Hardware</h2>
<p>
	The study runs on x86-64 bare metal, the architecture of every system in the comparison
	literature, so results compare directly to prior work.
</p>
<p>
	Primary testbed: two CloudLab c6525-100g nodes (Utah cluster).
	Per node: one AMD EPYC 7402P, 24 cores at 2.80 GHz, Zen 2; 128 GB ECC DDR4-3200 (8 × 16 GB
	RDIMM); two 1.6 TB NVMe SSDs, PCIe 4.0; one 25 GbE and one 100 GbE experiment link.
	One NVMe device holds the system and results; the second is dedicated to the store under test,
	so guest IO and compaction never share a device with the OS.
	The 100 GbE pair carries S3.
	CloudLab allocations are free for sponsored academic research; the sponsor approves the project.
</p>
<p>
	Fallback if CloudLab access is not granted: two OVHcloud Advance bare-metal servers (2026
	line), AMD EPYC 4005-series, 16 cores/32 threads, DDR5 ECC, 2 × 960 GB NVMe, 25 Gbps private
	bandwidth.
</p>
<p>
	Neither testbed has RDMA NICs, persistent memory, or accelerators; the commodity restriction is
	part of the claim.
	Every figure in the paper is measured on the testbed rather than quoted.
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
