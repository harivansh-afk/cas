<script lang="ts">
	import PageHead from '$lib/components/PageHead.svelte';
	import PageNav from '$lib/components/PageNav.svelte';
	import { Diagram, Node, Edge, Group, Note } from '$lib/components/diagram';
</script>

<PageHead num="00" />
<p class="lede">
	<mark>Deduplication tables in ZFS and dm-vdo find equal blocks within one host, and nothing they know leaves it. This study makes a chunk's hash its address, so every host can tell from the hash alone where a chunk lives, whether a peer already holds it, and where it is cached. Deduplication therefore crosses hosts. We build that as a block backend under unmodified QEMU and measure on two hosts what it gains, provisioning and migration that move only a manifest, one copy per chunk across the fleet, and a peer's memory as a cache, against what a cold read over the network costs.</mark><br />
	In OpenZFS and dm-vdo the hash of a block is a key in a side table, and the block is still addressed by its location on disk.<br />
	In a content-addressed store the hash is the address. Two hosts that hold the same bytes compute the same address without speaking, so placement, transfer, and the cache key can follow the content rather than the host.
</p>
<p>
	Three things follow, and each is measured. A guest is provisioned or migrated by moving its manifest, because the chunks it names already exist at their owners. Each unique chunk is stored k times across the fleet instead of once per host. A chunk is served by its owner, and from the owner's memory when it is hot, because every host reads it there.<br />
	Two things are paid, and both are measured. A cold read of a chunk another host holds costs one round trip on the network. Durability before acknowledgment becomes a choice between this host's disk alone and a peer's disk as well.<br />
	The system is a content-addressed block backend under unmodified QEMU. Its scope, called the testbed from here on:
</p>
<ul class="plain">
	<li>two hosts with static membership</li>
	<li>Linux guests</li>
	<li>single-digit terabytes</li>
</ul>

<h2>Deduplication within a host</h2>
<p>
	Consider two guests that each run <code>apt upgrade</code> and download the same packages. Their disks now hold the same bytes, and no copy-on-write clone can share them, because neither copy descends from the other.
</p>
<p>
	A deduplication table shares them. OpenZFS keeps one per pool, the DDT, and Linux has had <a href="https://docs.kernel.org/admin-guide/device-mapper/vdo.html" target="_blank" rel="noopener">dm-vdo</a> in the mainline kernel since 6.9.<br />
	Both hash every block and share equal blocks at one fixed, aligned size: 4 KiB for dm-vdo, and the volblocksize of a ZFS zvol, <a href="https://openzfs.github.io/openzfs-docs/man/master/7/zfsprops.7.html" target="_blank" rel="noopener">16 KiB by default</a>.<br />
	This study calls the guest's 4 KiB unit a block and the store's unit a chunk, whether the chunk's boundaries are fixed or content-defined.
</p>
<p>
	Nearly everything a Linux guest writes is 4 KiB aligned. ext4 uses 4 KiB blocks, partitions start at 1 MiB, and package managers write whole files.<br />
	<a href="https://ssrc.us/media/pubs/082a25b906aa716ca3c2439b8c1889449ecac44c.pdf" target="_blank" rel="noopener">Jin and Miller</a> found on VM disk images that fixed-size chunks reach nearly the same deduplication ratio as content-defined chunking (CDC), which places chunk boundaries by the bytes themselves. <a href="https://madsys.cs.tsinghua.edu.cn/publications/TPDS2014-zhao.pdf" target="_blank" rel="noopener">Liquid</a> measured 77% of bytes removed at 4 KiB fixed blocks on 183 images.
</p>
<p>
	<mark>We therefore predict that on one host the backend stores within 10% of what ZFS fast dedup stores when its chunk size equals the zvol's block size.</mark><br />
	Page 02 tests this as hypothesis 1.
</p>
<p>
	None of this reaches across hosts.<br />
	The DDT is per pool, dm-vdo has no replication, and <code>zfs send</code> has not carried a deduplicated stream since <a href="https://github.com/openzfs/zfs/issues/7887" target="_blank" rel="noopener">OpenZFS 2.0</a>.<br />
	A fleet of N hosts, each with its own table, stores a chunk shared by all of them N times. Moving a guest to another host sends every block of its image, whether or not the destination holds an equal one.
</p>
<p>
	Shared-storage systems deduplicate across hosts by placing every write on the network before it is acknowledged. Ceph RBD with <a href="https://www.usenix.org/system/files/atc23-oh.pdf" target="_blank" rel="noopener">TiDedup</a> is the open example.<br />
	Page 06 lists the systems on either side.
</p>

<Diagram
	w={960}
	h={284}
	label="Left: two hosts each with their own deduplication table and no link between them; the same chunk is stored on both and moves whole when a guest migrates. Right: two hosts whose chunks are named by hash and owned by hash across both; a chunk is stored k times fleet-wide, a guest's manifest moves while its chunks stay, and a cold read fetches by hash from the owner."
	caption="Left: a deduplication table indexes blocks within one pool. Right: a hash is the same name on every host, so placement, transfer, and the cache key follow the content."
>
	<Group x={20} y={20} w={440} h={244} label="per-host deduplication" />
	<Node x={40} y={56} w={195} h={44} title="host A" sub="guests on ZFS" tone="muted" />
	<Node x={40} y={116} w={195} h={44} title="DDT A" sub="hash → block address in pool A" />
	<Node x={245} y={56} w={195} h={44} title="host B" sub="guests on ZFS" tone="muted" />
	<Node x={245} y={116} w={195} h={44} title="DDT B" sub="hash → block address in pool B" />
	<Note x={240} y={190} anchor="middle" tone="muted" text={['the two tables are independent', 'a shared chunk is stored on both hosts', 'migration copies the whole image']} />

	<Group x={500} y={20} w={440} h={244} label="content addressing" tone="accent" />
	<Node x={520} y={56} w={180} h={44} title="host A" sub="guests on the daemon" tone="muted" />
	<Node x={520} y={116} w={180} h={44} title="chunks owned by A" sub="owner chosen by hash" tone="accent" />
	<Node x={740} y={56} w={180} h={44} title="host B" sub="guests on the daemon" tone="muted" />
	<Node x={740} y={116} w={180} h={44} title="chunks owned by B" sub="owner chosen by hash" tone="accent" />
	<Edge points={[[700, 130], [740, 130]]} tone="accent" />
	<Edge points={[[740, 146], [700, 146]]} tone="accent" />
	<Note x={720} y={110} anchor="middle" tone="accent" size={10} text="GET and PUT by hash" />
	<Note x={720} y={190} anchor="middle" tone="accent" text={['one chunk namespace across hosts', 'a shared chunk is stored k times fleet-wide', 'migration copies the manifest; the chunks stay', 'a cold read fetches the chunk from its owner']} />
</Diagram>

<h2>What is gained across hosts</h2>
<p>
	A chunk named by its hash has that name on every host, so its placement, its transfer, and its cache key are functions of its content.<br />
	Each consequence below is a measured claim on pages 03 and 04.
</p>
<p>
	<strong>Transfer.</strong><br />
	Provisioning a guest from an image whose chunks exist in the fleet moves the manifest, at least 32 bytes per chunk, and no chunk data.<br />
	Migrating a guest moves the manifest plus the bytes written since the last compaction.<br />
	Migration is measured because it is the operation in which "only unique bytes cross the wire" is easiest to isolate.
</p>
<p>
	<strong>Capacity.</strong><br />
	The fleet stores each unique chunk k times rather than once per host, and each host's index holds entries only for the chunks it owns, plus surplus copies until their owner acknowledges them.
</p>
<p>
	<strong>Cache.</strong><br />
	Every host sends its reads of a chunk to the same k owners. We predict that a chunk many guests read is therefore hot at its owner, and page 04 reports the owner's hit rate.<br />
	On ConnectX-5 hardware, a 4 KiB read against a null target, the fabric and kernel stack with no media beneath them, measured 12 µs over kernel nvme-rdma and 21 µs over kernel nvme-tcp (<a href="https://review.spdk.io/download/performance-reports/SPDK_rdma_mlx_perf_report_2405.pdf" target="_blank" rel="noopener">SPDK 24.05</a>). A 4 KiB read from an enterprise NVMe SSD measured about 80 µs (<a href="https://www.systor.org/2017/slides/NVMe-over-Fabrics_Performance_Characterization.pdf" target="_blank" rel="noopener">Systor '17</a>; <a href="https://www.usenix.org/system/files/osdi21-hwang.pdf" target="_blank" rel="noopener">blk-switch</a>).<br />
	<mark>If those figures hold on the testbed, a chunk in a peer's memory arrives before a chunk on the local disk, since the memory read adds little to the stack.</mark> Hypothesis 3 tests this.
</p>

<h2>What is paid</h2>
<p>
	Three costs come with any post-process deduplicating store, and each is measured in this study:
</p>
<ul class="plain">
	<li>write amplification, because every surviving byte is written to the staging log and again to the store</li>
	<li>compactor interference, because compaction shares the guest's disk</li>
	<li>index memory, one entry per chunk in RAM</li>
</ul>
<p>
	One cost belongs to distribution alone. For a chunk this host does not hold, the network is on the read path.<br />
	Page 04 measures that read over TCP and over RDMA, from the peer's memory and from the peer's NVMe.<br />
	It then measures prefetch. The daemon reads the manifest, so it knows which chunks a sequential reader will ask for next and fetches them while the guest works on the last one.<br />
	How much of the cost prefetch removes is a reported number in this study.
</p>
<p>
	One cost is a tradeoff the design makes on purpose: durability before acknowledgment.<br />
	The bytes a guest has written must be on disk, not in memory, before the daemon acknowledges its FLUSH.<br />
	A guest's FLUSH, the guest's fsync, requires that the bytes survive a crash of the daemon, of QEMU, and of the host.<br />
	In local class, the default, the daemon acknowledges after fdatasync on this host, which is the contract a local disk gives. If the host is lost before compaction has shipped those bytes to their owners, they are lost with it.<br />
	In fleet class the daemon also sends the bytes themselves to one fixed peer, the journal peer (not the manifest, since the manifest names bytes that exist nowhere else yet) and acknowledges after both this host's fdatasync and the peer's. Fsynced bytes therefore survive the loss of this host.<br />
	<a href="https://www.nutanixbible.com/4g-book-of-aos-data-io-path.html" target="_blank" rel="noopener">Nutanix AOS</a> replicates its write log to another node before it acknowledges, and <a href="https://experistg.com/wp-content/uploads/2019/12/The-technology-enabling-HPE-SimpliVity-data-efficiency.pdf" target="_blank" rel="noopener">HPE SimpliVity</a> mirrors every write between two nodes. Nutanix, Datrium, vSAN ESA, and SimpliVity each acknowledge after a network copy.<br />
	Page 04 measures the price of the difference: one round trip and one remote fdatasync per FLUSH, over TCP, and over RDMA if that arm lands.
</p>

<h2>Hypotheses</h2>
<p class="note">
	Each hypothesis states a metric with its conditions, a comparator, a threshold, the source of the threshold, and what a miss would show.<br />
	Thresholds are frozen at the end of week 2, after R0 has measured the testbed's fdatasync and media times, and do not move after that.
</p>
<p>
	<strong>1. Single-host parity.</strong><br />
	Bytes stored by the backend after compaction and sweep, under the fleet replay at fixed 4 KiB and 16 KiB, are within 10% of the bytes ZFS fast dedup stores at the same volblocksize. Bytes stored in each chunk-size arm are within 10% of the census prediction for that arm.<br />
	Guest write and read p99 at 4 KiB QD1, with the compactor idle and again with it active, are within 20% of a raw file on XFS.<br />
	The 10% is the alignment argument above plus record headers, since the sweep runs before every capacity number. The 20% is the passthrough bound of gate G1 plus an equal allowance for the log append and compactor interference.<br />
	A miss on capture would show that fixed aligned chunks lose duplicates a Linux guest produces. A miss on p99 would show that the host daemon (not deduplication) is the cost paid.
</p>
<p>
	<strong>2. Transfer and capacity across hosts.</strong><br />
	Bytes on the wire to provision a guest are within 10% of its manifest size, and to migrate one within 10% of the manifest plus the staging tail, against the allocated image size that <code>zfs send</code> or rsync moves.<br />
	Bytes sent to synchronize two drifted guests are within 10% of the census's unique-byte count for the pair.<br />
	Bytes stored on both hosts in partitioned mode, after the sweep, are at most 55% of the bytes two per-host ZFS pools hold for the same guests.<br />
	The 10% on transfer covers framing and HAS replies, since no chunk an owner holds is sent by design. The 55% is one copy of the unique set instead of two, because guests cloned from one image give the two pools nearly the same unique set, plus five points for record headers and manifests.<br />
	A miss on transfer would mean chunks were sent that an owner already held, a HAS or fence defect. A miss on capacity would mean the two pools shared less than the census predicted, which the census would show first.
</p>
<p>
	<strong>3. Reads over the wire.</strong><br />
	For a 4 KiB read at QD1 whose chunk is not in the local cache, over the daemon on kernel TCP:<br />
	Served from the owner's memory, guest-visible latency is lower than the same read served by the daemon from its own NVMe.<br />
	Served from the owner's NVMe, it is at most 40% over the local read.<br />
	With reads in flight at or above the bandwidth-delay point, remote sequential throughput is within 10% of local.<br />
	In a partitioned boot storm of 16 guests with profile prefetch, guest p99 is within 25% of the same storm in replicated mode.<br />
	The kernel nvme-rdma probe, which stands in for a daemon over RDMA, serves the owner's NVMe at most 15% over the local read. The probe is not the architecture, and its number is reported as the floor the ibverbs arm could approach.<br />
	The thresholds are the literature stack on page 04: about 80 µs of media, plus 20 to 30 µs for a userspace daemon over kernel TCP and about 12 µs for kernel nvme-rdma.<br />
	A miss on the first part would show that the kernel stack or the daemon's wakeup costs more than the media. A miss on throughput would show that the fabric bounds it. A miss on the boot storm would show that prefetch does not hide the remote read under a real access pattern.
</p>
<p>
	<strong>4. Durability before acknowledgment.</strong><br />
	Write p99 at 4 KiB QD1 in fleet class is within 3x of local class over TCP, and within 2x over RDMA if the ibverbs arm lands.<br />
	The window of local class, the seconds between a FLUSH acknowledgment and the durability of those bytes at their owners, is reported as a distribution under the fleet replay.<br />
	The 3x is one round trip plus one peer fdatasync alongside the local fdatasync, on the page 04 figures and an fdatasync near 40 µs (NEED DATA; measured on the testbed drive in week 1).<br />
	A miss would show that the journal path rather than the transport is the cost. The peer's fdatasync time is reported separately so the two can be told apart.
</p>

<h2>Results</h2>
<p>
	<strong>The system.</strong><br />
	A content-addressed block backend for VMs under unmodified QEMU on a stock Linux kernel, over kernel TCP, with source, configuration, and the scripts that produce every table.
</p>
<p>
	<strong>The single-host table.</strong><br />
	The backend against ZFS fast dedup and a raw file on XFS: bytes stored, guest p99, write amplification, and index memory, at three chunk sizes.<br />
	Hypothesis 1 is decided here. The capture against index memory curve across the three arms is reported without a threshold, because no prior curve on NVMe exists to bound it.
</p>
<p>
	<strong>The multi-host table.</strong><br />
	Bytes moved to provision and to migrate a guest, bytes sent to synchronize two drifted guests, fleet bytes stored with one copy per chunk, and index bytes per host, each against what <code>zfs send</code> or rsync moves and what two per-host ZFS pools hold.
</p>
<p>
	<strong>The remote-read measurement.</strong><br />
	A content-addressed chunk fetched from a peer under a VM block device, at microsecond resolution, over the daemon on kernel TCP and over NVMe-oF on TCP and RDMA, from the peer's memory and from its NVMe, with and without prefetch.
</p>
<p>
	<strong>The durability trade.</strong><br />
	Local class against fleet class on the same hardware: the write latency fleet class costs per transport, and the seconds of acknowledged data local class puts at risk.
</p>

<h2>Scope</h2>
<p>
	The study covers hosts that serve guests from local flash, from a small homelab setup up to rack scale (storage arrays and hyperscale economics are out of scope).<br />
	Hosts hold each other's chunks, which couples the failure domains of compute and storage that shared-storage designs keep apart.<br />
	This study measures what that costs on the read path and does not model its availability.
</p>
<p>
	The testbed is two hosts with static membership. Membership changes, failure detection, rebalancing, authentication and encryption on the wire, measurement on more than two hosts, and concurrent garbage collection are out of scope, and none affects a number reported here.<br />
	One copy per chunk (k = 1) on two hosts sends the largest share of cold reads over the network that this testbed can produce. In general the share is 1 − k/N.<br />
	A deployment runs k ≥ 2 on N ≥ 3 hosts.
</p>
<p>
	Each image has only one writer.<br />
	Ownership state, the root record that names the writer and carries a generation number, is held on both hosts. Two hosts form no quorum, so failover of a lost writer is a scripted decision and not automatic.<br />
	The study migrates disks only. Memory migration is QEMU's own live migration.
</p>
<p>
	The guest contract is virtio-blk with a volatile write cache: an acknowledged FLUSH is durable and nothing else is.<br />
	Every configuration runs with QEMU <code>cache=none</code>, so the host page cache is bypassed everywhere.
</p>
<p>
	Equal BLAKE3 hashes are taken to mean equal bytes. A sample of matches is verified byte for byte and the sample size is reported.<br />
	The store is trusted infrastructure, so deduplication side channels are documented and excluded.
</p>
<p>
	Experiments run at single-digit TB, and larger figures are projections from measured constants, labeled as such.<br />
	RDMA is a measurement arm only and not a prerequisite for this study.
</p>

<PageNav num="00" />
