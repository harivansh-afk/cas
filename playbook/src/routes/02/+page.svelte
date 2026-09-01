<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="02" />
<p class="lede">
	<strong>Local write, global dedup.</strong> The write path never crosses the network: staging is
	a local log on the host where the guest runs, so ingest latency is a local NVMe question no
	matter how large the cluster. Content addressing goes global at compaction, where deferred work
	belongs.
</p>

<h2>What distributes for free</h2>
<p>
	Chunks are immutable and named by content, so placement is a pure function of the name:
	rendezvous or CRUSH-style hashing from chunk hash to k owner nodes. No allocation tables, no
	rebalancing metadata, no coordinator on the data path. The index partitions by the same
	function, so the shard that owns a chunk owns its index entry; lookup and placement are one
	computation. Any node may cache any chunk, and the cache is convergent cluster-wide, the
	page-cache sharing effect at rack scale. Verification travels with the data: scrub is re-hash,
	and a corrupt replica is repairable from any peer, provably, by name.
</p>
<p>
	Compaction ships only chunks the owning shard does not already hold. Cluster ingest cost is
	therefore proportional to unique bytes, not written bytes. That is the O(delta) property of
	content addressing, surfacing at the fabric level, and it is the reason H3 belongs in the thesis
	rather than the future-work section.
</p>

<h2>What stays hard</h2>
<p>Honesty about the other half:</p>
<ul class="reqs">
	<li>
		<strong>Maps are mutable and stay with their writer.</strong> One image, one writer (A5), so
		the map lives on the host running the guest and moves only when the guest does. Map placement
		is lineage-shaped even when data placement is not.
	</li>
	<li>
		<strong>Global liveness.</strong> The sweep needs roots from every map owner. Epoch-based
		collection, roots gathered per epoch, nothing reclaimed inside an open epoch. Designed here;
		validated only at small scale.
	</li>
	<li>
		<strong>Remote read p99.</strong> A cold read whose chunk lives elsewhere pays a network round
		trip inside the guest's latency. Staging absorbs writes and recent reads; hot-chunk caching
		absorbs some of the rest; what remains is the real cost of disaggregation, and measuring it
		properly over NVMe/TCP is the designated follow-on study (the i10 lineage), not this semester.
	</li>
	<li>
		<strong>Index locality at scale.</strong> Partitioning solves placement, not RAM. The per-TB
		index constants measured in S2 are what make any cluster-scale extrapolation honest.
	</li>
</ul>
<p>
	This page argues H3 from the design and demonstrates it at two nodes: correct placement by hash,
	and a sync whose transferred bytes track unique bytes. Everything else on this page is labeled
	phase 2.
</p>

<PageNav num="02" />
