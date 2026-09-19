<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
</script>

<PageHead num="06" />
<p class="lede">
	Swept on 2026-09-01. Sources and what was opened are in <code>docs/review/</code>.<br />
	No system in the sweep combines a durable, sequence-numbered local write log with a stated FLUSH contract, a fleet-wide chunk store whose owners are the hosts themselves, a block device under a stock hypervisor, and a per-transport measurement of the remote cold read.<br />
	Liquid (TPDS '14) is the nearest design and the row to read first. Datrium, Nutanix, and Fossil with Venti each share one component.
</p>

<h2>Nearest systems</h2>
<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Work</th><th>What it is</th><th>How this differs</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">Datrium DVX (2016), US20170031994A1</td><td>host-side fingerprinting, host flash as read cache, global deduplication on a shared data-node pool; the patent lists host-only ack as an alternative</td><td>peers as owners by hash instead of a shared pool; open implementation on stock QEMU; the cold read measured per transport</td></tr>
			<tr><td class="k">Nutanix AOS</td><td>local OpLog on SSD, mirrored to another node before ack; cluster-wide post-process deduplication at 16 KiB; per-node cache</td><td>no mirror on the write path by default, with the window measured and the mirror as fleet class; placement by hash instead of by vDisk locality; latency and capacity numbers, which its documentation does not give</td></tr>
			<tr><td class="k">Fossil + Venti (2002)</td><td>a disk write buffer in front of a content-addressed archive; the two-tier shape</td><td>block device under a VM instead of a filesystem; primary capacity instead of archival; more than one owner</td></tr>
			<tr><td class="k">Ceph + TiDedup (ATC '23)</td><td>post-process CDC into a chunk pool placed by CRUSH on the fingerprint; promotes on a cold miss</td><td>guest writes are acknowledged before any byte crosses the network; a host cache instead of promotion; a guest block path; latency numbers, which TiDedup does not report</td></tr>
			<tr><td class="k">vSAN ESA global deduplication (2025)</td><td>cluster-wide post-process 4 KiB deduplication, mirrored writes, 3 to 16 hosts, no published numbers</td><td>the per-host to cluster-wide change this study measures, with published numbers</td></tr>
			<tr><td class="k">HYDRAstor (FAST '09)</td><td>content-addressed blocks placed by DHT across a grid, global deduplication</td><td>secondary storage with network writes; no guest path</td></tr>
			<tr><td class="k">DeDe (ATC '09)</td><td>hosts hash in-band, deduplicate out-of-band against a shared index on a SAN, no coordinator</td><td>local disks instead of a SAN; chunks move to owners instead of pointers on shared storage</td></tr>
			<tr><td class="k"><a href="https://madsys.cs.tsinghua.edu.cn/publications/TPDS2014-zhao.pdf" target="_blank" rel="noopener">Liquid (TPDS '14)</a></td><td>FUSE file under a stock hypervisor; fixed 256 KiB to 1 MiB blocks hashed on flush or eviction from a 256 MB volatile write cache, pushed to range-partitioned data servers at VM shutdown; central meta server with refcounts; P2P Bloom-filter cache tier; copy-on-read disk cache; two replicas</td><td>a durable log with a FLUSH contract instead of a volatile buffer with no crash story; a vhost-user block device instead of FUSE; hosts as owners by rendezvous instead of a meta server and a data-server tier; exact HAS instead of Bloom filters; the miss cost measured, which Liquid names ("several times longer") and never measures</td></tr>
		</tbody>
	</table>
</div>

<h2>Remote fetch in prior systems</h2>
<div class="table-scroll">
	<table class="spec">
		<thead>
			<tr><th>Work</th><th>What it measured</th><th>What it leaves open</th></tr>
		</thead>
		<tbody>
			<tr><td class="k">Liquid (TPDS '14)</td><td>8 GB image to 7 nodes on 1 GbE: scp 730 s, NFS 510 s, BitTorrent 95 s, Liquid 35 s; on-demand boot 1.7x to 4x a cached boot</td><td>miss cost stated as "several times longer IO delay" and never measured; no latency numbers anywhere; HDD and 1 GbE</td></tr>
			<tr><td class="k">DADI (ATC '20)</td><td>block-level lazy loading with tree P2P; 10,000 containers on 1,000 hosts in 4 s; trace prefetch removes 95% of the cold gap; reads from a parent's page cache are faster than local disk</td><td>no per-read miss latency; not content-addressed</td></tr>
			<tr><td class="k">Slacker (FAST '16)</td><td>only 6.4% of a container image is read at startup; lazy fetch over NFS; run phase 17% slower</td><td>no per-block miss cost; centralized</td></tr>
			<tr><td class="k">VMTorrent (CoNEXT '12), VMThunder (TPDS '14)</td><td>demand-priority P2P VM image streaming; VMTorrent replays recorded profiles, VMThunder streams on demand through a relay tree</td><td>startup seconds only</td></tr>
			<tr><td class="k">FaaSnap (EuroSys '22), REAP (ASPLOS '21)</td><td>lazy page faults from local disk at 13 µs; userfaultfd over 128 µs uncached; working set 9% of footprint</td><td>memory, not disk; local</td></tr>
			<tr><td class="k">SnowFlock (EuroSys '09)</td><td>275 µs per page fetched over gigabit, 82% of it in the network stack</td><td>the only in-VM remote per-unit number, and it is from 2009</td></tr>
			<tr><td class="k">Dahlin et al. (OSDI '94)</td><td>cooperative caching: remote client memory at 1.25 ms against disk at 15 ms; N-chance forwarding</td><td>the argument this study repeats at 100 GbE with content-addressed chunks</td></tr>
			<tr><td class="k">CLB (VEE '17), Satori (ATC '09)</td><td>content-keyed sharing of VM disk reads across guests on one host; 94.9 to 98.5% of boot reads eliminated</td><td>single host; no store</td></tr>
		</tbody>
	</table>
</div>
<p>
	<mark>Among the systems above, which span FAST, ATC, OSDI, EuroSys, ASPLOS, CoNEXT, VEE, and TPDS from 1994 to 2022, none reports the latency of a content-addressed chunk fetched from a peer inside a VM block read path.</mark><br />
	DADI, VMTorrent, REAP, and FaaSnap report startup time in seconds and hide the per-read penalty behind a recorded access profile, and VMThunder reports seconds without one. Liquid names the penalty and does not measure it.
</p>

<h2>Objections already in print</h2>
<p>
	<strong>Dong et al. (FAST '11)</strong> rejected per-chunk hash placement for backup streams on locality grounds and routed 1 MB super-chunks. Page 03 answers with a local cache and page 04 measures the cost.<br />
	<strong>Meyer and Bolosky (FAST '11)</strong> found that deduplication savings grow with the log of the number of file systems in one domain, on 857 desktops. Two hosts is the floor of the capacity half of hypothesis 2, and a larger fleet gains more.<br />
	<strong>despairlabs (2024)</strong> tells ZFS operators to use clones and block cloning for the copy case and deduplication rarely. Hypothesis 1 predicts agreement on one host, and hypothesis 2 measures the cross-host case that advice does not address.<br />
	<strong>The hyperconverged products</strong> (Nutanix, Datrium, SimpliVity, vSAN ESA) mirror a write over the network before acknowledging it, so a local-only acknowledgment is a durability trade rather than a free latency gain. Page 01 makes it a class and page 04 prices it.
</p>

<h2>What this study adds</h2>
<p>
	The study's contribution is the measurement: what content addressing provides across hosts on commodity hardware under a stock hypervisor, and what the remote cold read costs, per transport.
</p>

<PageNav num="06" />
