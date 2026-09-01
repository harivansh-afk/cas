<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="06" />
<p class="lede">
	Swept on 2026-09-01; sources and what was actually opened are in <code>docs/review/</code>.<br />
	No prior system is a local-only write log with no network on the write path, a fleet-wide hash-placed chunk store, and remote cold reads under a stock hypervisor.<br />
	Three are close enough that a reviewer would write a sentence if they were missing.
</p>

<h2>Nearest systems</h2>
<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Work</th><th>What it is</th><th>How this differs</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">Datrium DVX (2016), US20170031994A1</td><td>host-side fingerprinting, host flash as read cache, global dedup on a shared data-node pool; the patent lists host-only ack as an alternative</td><td>peers as owners by hash instead of a shared pool; open implementation on stock QEMU; the cold read priced per transport</td></tr>
			<tr><td class="k">Nutanix AOS</td><td>local OpLog on SSD, mirrored to another node before ack; cluster-wide post-process dedup at 16K; per-node cache</td><td>no mirror on the write path, with the window measured and the mirror as an arm; placement by hash instead of by vDisk locality; numbers published</td></tr>
			<tr><td class="k">Fossil + Venti (2002)</td><td>a disk write buffer in front of a content-addressed archive; the two-tier shape</td><td>block device under a VM instead of a filesystem; primary capacity instead of archival; more than one owner</td></tr>
			<tr><td class="k">Ceph + TiDedup (ATC '23)</td><td>post-process CDC into a chunk pool placed by CRUSH on the fingerprint; promotes on a cold miss</td><td>writes never cross the network; a host cache instead of promotion; a guest block path; latency numbers, which TiDedup does not report</td></tr>
			<tr><td class="k">vSAN ESA global dedup (2025)</td><td>cluster-wide post-process 4K dedup, mirrored writes, 3 to 16 hosts, no published numbers</td><td>the per-host to cluster-wide change this study measures, in the open</td></tr>
			<tr><td class="k">HYDRAstor (FAST '09)</td><td>content-addressed blocks placed by DHT across a grid, global dedup</td><td>secondary storage with network writes; no guest path</td></tr>
			<tr><td class="k">DeDe (ATC '09)</td><td>hosts hash in-band, dedup out-of-band against a shared index on a SAN, no coordinator</td><td>local disks instead of a SAN; chunks move to owners instead of pointers on shared storage</td></tr>
			<tr><td class="k">Liquid (TPDS '14)</td><td>fingerprint-keyed VM image filesystem, P2P fetch across hosts, copy-on-read local cache</td><td>block device under a stock hypervisor instead of a filesystem; owner by hash instead of P2P; full text not yet read</td></tr>
		</tbody>
	</table>
</div>

<h2>Remote fetch</h2>
<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Work</th><th>What it measured</th><th>What it leaves open</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">DADI (ATC '20)</td><td>block-level lazy loading with tree P2P; 10,000 containers on 1,000 hosts in 4 s; trace prefetch removes 95% of the cold gap; reads from a parent's page cache beat local disk</td><td>no per-read miss latency; not content-addressed</td></tr>
			<tr><td class="k">Slacker (FAST '16)</td><td>only 6.4% of a container image is read at startup; lazy fetch over NFS; run phase 17% slower</td><td>no per-block miss cost; centralized</td></tr>
			<tr><td class="k">VMTorrent (CoNEXT '12), VMThunder (TPDS '14)</td><td>demand-priority P2P VM image streaming with recorded profiles</td><td>startup seconds only</td></tr>
			<tr><td class="k">FaaSnap (EuroSys '22), REAP (ASPLOS '21)</td><td>lazy page faults from local disk at 13 µs; userfaultfd over 128 µs uncached; working set 9% of footprint</td><td>memory, not disk; local</td></tr>
			<tr><td class="k">SnowFlock (EuroSys '09)</td><td>275 µs per page fetched over gigabit, 82% of it in the network stack</td><td>the only in-VM remote per-unit number, and it is from 2009</td></tr>
			<tr><td class="k">Dahlin et al. (OSDI '94)</td><td>cooperative caching: remote client memory at 1.25 ms beats disk at 15 ms; N-chance forwarding</td><td>the argument this study remakes at 100 GbE with content names</td></tr>
			<tr><td class="k">CLB (VEE '17), Satori (ATC '09)</td><td>content-keyed sharing of VM disk reads across guests on one host; 95 to 98% of boot reads eliminated</td><td>single host; no store</td></tr>
		</tbody>
	</table>
</div>
<p>
	<mark>Nobody has measured a content-addressed chunk fetched from a peer inside a VM block read path at microsecond scale.</mark><br />
	Every lazy-loading system reports startup seconds, admits a per-read penalty, and hides it with a recorded prefetch profile.
</p>

<h2>Transport</h2>
<p>
	i10 (NSDI '20) and blk-switch (OSDI '21) showed kernel TCP can match RDMA on throughput per core with batching, at a latency cost of 50 to 100 µs at low load.<br />
	The SPDK 24.05 reports on ConnectX-5 put kernel nvme-rdma at 12.1 µs and kernel nvme-tcp at 21.4 µs for a 4K read against a null device.<br />
	Homa (ATC '21) and eRPC (NSDI '19) put kernel bypass at 2 to 4 µs and attribute the rest of kernel TCP to wakeups and core selection.<br />
	No storage paper measured a non-spinning userspace daemon over kernel TCP as a remote read target; that row is estimated on page 04 and measured here.
</p>

<h2>Objections already in print</h2>
<p>
	<strong>Dong et al. (FAST '11)</strong> rejected per-chunk hash placement for backup streams on locality grounds and routed 1 MB super-chunks; page 03 answers with a local cache and page 04 measures the cost.<br />
	<strong>Meyer and Bolosky (FAST '11)</strong> already showed dedup savings grow with the log of the number of machines in one domain, which is the capacity half of H2 stated for desktops.<br />
	<strong>Jin and Miller (SYSTOR '09)</strong> found fixed blocks match CDC on VM images, which is why part 1 predicts a tie.<br />
	<strong>despairlabs (2024)</strong> tells ZFS operators to use clones and block cloning for the copy case and dedup rarely; the study agrees on one host and disagrees across hosts.
</p>

<h2>What remains</h2>
<p>
	Datrium's patent and Nutanix's design are cited by name.<br />
	Fossil and Venti are cited as the origin of the two-tier shape.<br />
	The study's claim is the measurement: what a name buys across hosts on commodity hardware under a stock hypervisor, and what the cold read costs, per transport, with the numbers.
</p>

<PageNav num="06" />
