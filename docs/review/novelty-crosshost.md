# Novelty check: local-write, globally deduplicated VM block storage

Date: 2026-09-01. Scope: the proposed design of a content-addressed block backend for VMs on stock QEMU (vhost-user-blk to a userspace daemon), where guest writes land in a local staging log, a compactor chunks and hashes settled data with BLAKE3, and unique chunks are placed across hosts by hash so dedup is fleet-wide with k copies per unique chunk. Cold reads of remote chunks fetch from the owner.

## Verdict

I found no prior system that is exactly this: a local-only append log with no network on the write path, a fleet-wide hash-placed chunk store, and remote cold reads, under a stock hypervisor. The three closest are Datrium DVX and its patent, Nutanix AOS, and Plan 9 Fossil plus Venti. Liquid (TPDS 2014) and Ceph's dedup pool with TiDedup are the closest open designs on the read and placement side.

The first objection a reviewer will raise is not novelty. Every production system in this space mirrors the write to another node before acknowledging it. "No network on the write path" is a durability trade: a host that dies before compaction loses its staged writes. The study has to say so and measure the window, or the latency claim reads as an omission rather than a design.

The session's web search budget ran out partway through. Items marked [not opened] rest on abstracts, search snippets, or memory.

## 1. Commercial hyperconverged and scale-out

### Nutanix AOS

- Sources: [Nutanix Bible, Data I/O Path](https://www.nutanixbible.com/4g-book-of-aos-data-io-path.html); [Nutanix Bible, Data Efficiency](https://www.nutanixbible.com/4h-book-of-aos-data-efficiency.html).
- What it is: the OpLog is a local SSD write log, but "upon a write, the OpLog is synchronously replicated to another n number of CVM's OpLog before the write is acknowledged". Dedup fingerprints at ingest on 16KB chunks inside 1MB extents (SHA-1 before AOS 5.11, "logical checksums" after). An extent is deduped when more than 40% of its chunks match (per-chunk marking since AOS 6.6). Removal is post-process through Curator MapReduce, cluster-wide. The Unified Cache is per CVM, 4K granularity, and holds dedup data.
- Verdict: close. Local log, cluster-wide post-process dedup, per-node cache. Different in that the write is network-mirrored before ack and extents are placed by vDisk locality, not by hash. No throughput or latency numbers published in the Bible.

### VMware vSAN

- Sources: [vSAN OSA dedup and compression (Broadcom techdocs)](https://techdocs.broadcom.com/us/en/vmware-cis/vsan/vsan/8-0/vsan-administration/increasing-space-efficiency-in-a-vsan-cluster/using-deduplication-and-compression-in-vsan-cluster.html); [Global Deduplication in vSAN ESA for VCF 9.0, VMware blog, 2025-06-19](https://blogs.vmware.com/cloud-foundation/2025/06/19/global-deduplication-in-vsan-esa-for-vmware-cloud-foundation-9-0/).
- What it is: OSA dedups per disk group only. ESA global dedup is post-process, hashes every 4KB block, keeps a cluster-wide dedup metadata object and a dedup data object, supports 3 to 16 hosts, requires 25GbE, and shipped as a limited release. The blog gives no numbers ("on par or better than deduplication found in many popular storage offerings").
- Verdict: close on cluster-wide post-process dedup. Writes are network-replicated. The OSA to ESA change is the literal per-host-domain versus cluster-domain case, and a reviewer will cite it.

### Datrium DVX (2016 to 2020)

- Sources: [StorageReview, DVX with Flash End-to-End](https://www.storagereview.com/review/datrium-dvx-with-flash-end-to-end-review); [Simon Long, What is Datrium DVX](https://www.simonlong.co.uk/blog/2018/07/26/what-is-datrium-dvx/); [myvirtualcloud, Datrium 3.0 part 2](http://myvirtualcloud.net/datrium-3-0-features-overview-beyond-marketing-part-2/); [US20170031994A1, System and methods for storage data deduplication](https://patents.google.com/patent/US20170031994A1/en). The vGeek TFD14 recap did not resolve [not opened].
- What it is: hosts fingerprint, dedup and compress; host flash is a read cache; writes are committed to data nodes (StorageReview measured 7.3 GB/s of network traffic during 4K random writes; Long says a write goes "simultaneously" to the compute node's flash and the data node). Data nodes hold an append-only log-structured store, erasure coded. The patent describes hosts buffering and sorting blocks into "clumps", fingerprinting each clump, a global clump index that dedups across vDisks, and a clump-based host cache. It also says "the new writes may also, or alternatively, be sent to one or more of the external storage nodes 300 and its NVRAM so that the data are not lost".
- Verdict: closest commercial system. Different in that the durable tier is a shared data-node pool, not peer hosts chosen by hash, and the shipping product acked writes from data-node NVRAM. The patent's "or alternatively" covers host-only acknowledgement, so the write path is weaker as a novelty claim than it first looks.

### HPE SimpliVity

- Sources: [The technology enabling HPE SimpliVity data efficiency (whitepaper PDF)](https://experistg.com/wp-content/uploads/2019/12/The-technology-enabling-HPE-SimpliVity-data-efficiency.pdf); [Futurum product analysis](https://futurumgroup.com/wp-content/uploads/documents/EGL1_HPE_SimpliVity-12.pdf).
- What it is: inline global dedup and compression at 4 to 8KB across the Federation, done at ingest by an accelerator card. "All VM data is mirrored between two nodes." Moving a VM or backup to another site sends metadata first and then only the data the remote site does not have.
- Verdict: close on global inline dedup and dedup-aware migration. The write is mirrored, and placement is per VM rather than per hash.

### Cisco HyperFlex / Springpath

- Sources: [Cisco blog, Introducing HyperFlex](https://blogs.cisco.com/datacenter/introducing-cisco-hyperflex-systems); [US11093464, Global deduplication on distributed storage using segment usage tables](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/11093464). The HX Data Platform whitepaper returned 403 [not opened].
- What it is: always-on inline dedup and compression on a distributed log-structured file system, up to 80% reduction claimed. The Springpath patent argues that a global multi-writer log across nodes dedups more than each writer deduping its own area.
- Verdict: different. Data is striped across nodes on write.

### Hedvig (Commvault)

- Sources: [Hedvig technical and architectural overview (PDF)](https://discover.commvault.com/rs/097-UGL-749/images/Whitepaper-Hedvig-Architecture-Overview%20.pdf); [US12547350, Global de-duplication of virtual disks in a storage platform](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/12547350), an image-only PDF I could not read.
- What it is: a storage proxy on the host keeps a "dedupe cache" of fingerprints on local SSD. If a written block is already known, the proxy updates the metadata service and "immediately sends a write acknowledgement back to the application" with no data crossing the network. Unique data goes to replica nodes; two of three replicas are written synchronously before ack. The whitepaper claims an average 75% reduction.
- Verdict: close on the idea that a host-side fingerprint cache short-circuits the network for duplicate writes. Unique data still crosses the network before ack.

### StorPool

- Source: [StorPool feature list](https://storpool.com/all-storpool-features).
- What it is: inline dedup is on the feature list. No architecture document found.
- Verdict: unknown, probably different [not opened deeper].

### Ceph and TiDedup

- Sources: [Ceph developer docs, Deduplication](https://docs.ceph.com/en/latest/dev/deduplication/); [TiDedup, USENIX ATC '23](https://www.usenix.org/conference/atc23/presentation/oh); [TiDedup slides](https://www.usenix.org/system/files/atc23_slides_oh.pdf); [Khan et al., cluster-wide dedup for shared-nothing storage, MASCOTS '18, arXiv 1803.07722](https://arxiv.org/abs/1803.07722).
- What it is: Ceph names a chunk object by its content hash, so CRUSH places chunks by hash across the cluster. Manifest objects redirect reads to the chunk pool. The docs say RBD objects see overwrites and "we don't want to pay a write latency penalty in the hot path". TiDedup adds a post-process crawler, content-defined chunking (FastCDC) and event-driven tiering. On 10.1 TB of vSphere images from 67 users, CDC saved 45/36/27% at 8/16/32K average chunk size, versus 21/12/10% for fixed chunks. Khan et al. place both chunks and dedup metadata by fingerprint on Ceph.
- Verdict: closest open-source design for "chunk placed by hash, read served by redirect". The write path is RADOS over the network.

### Pure FlashArray and VAST

- Sources: [VAST, Similarity reduction report from the field](https://www.vastdata.com/blog/similarity-reduction-report-from-the-field). Pure's dedup scope (per array) is from memory [not verified].
- What it is: VAST runs one reduction realm across the cluster, always on, with hash tables in storage-class memory; reported ratios run from 2:1 (genomics) to 8:1 (quantitative trading).
- Verdict: different. Shared storage, no local write log.

### Infinio Accelerator

- Sources: [Virtual Village, how Infinio works](https://virtualvillage.cloud/?p=590); [vladan.fr on Infinio 3.0](https://www.vladan.fr/infinio-accelerator-3-0-now-on-vaio-framework-with-up-to-1000000-iops-per-host/).
- What it is: a content-addressed, deduplicated RAM read cache shared across vSphere hosts (2013 to 2019).
- Verdict: only the "cache keyed by content across hosts" piece.

## 2. Academic

### HYDRAstor (FAST '09)

- Sources: [USENIX page](https://www.usenix.org/conference/fast09/technical-sessions/presentation/dubnicki); [paper PDF](https://www.usenix.org/legacy/event/fast09/tech/full_papers/dubnicki/dubnicki.pdf), opened.
- What it is: access nodes in front of a grid of storage nodes organised as a DHT (FPN with supernodes). Blocks are SHA-1 content-addressed, placed by hash prefix, globally deduplicated, erasure coded. Write throughput with dedup measured at 450 to 790 MB/s.
- Verdict: close on hash placement and remote fetch. No local staging log; writes cross the network to storage nodes. Backup workload.

### HydraFS (FAST '10)

- File system front end for HYDRAstor [not opened]. Different for the same reasons.

### Data Domain (Zhu, Li, Patterson, FAST '08)

- Source: [USENIX page](https://www.usenix.org/conference/fast-08/avoiding-disk-bottleneck-data-domain-deduplication-file-system).
- What it is: single-node inline dedup with stream-informed locality; 100 MB/s single stream, 210 MB/s multi-stream.
- Verdict: different.

### Dong et al., Tradeoffs in scalable data routing for deduplication clusters (FAST '11)

- Source: [paper PDF](https://www.usenix.org/legacy/event/fast11/tech/full_papers/Dong.pdf), opened.
- What it is: routes 1 MB super-chunks to nodes, stateless (hash of a feature) or stateful. Stateful routing stays within 10% of single-node dedup at 64 nodes (20% for some datasets). The stateless scheme shipped in a two-node product at 3 GB/s.
- Verdict: different, and adversarial. Dong rejects per-chunk hash placement because it destroys stream locality and multiplies index lookups. The study must explain why fine-grained hash placement is acceptable for primary VM storage (local cache absorbs read locality; the compactor is off the request path).

### Σ-Dedupe (Fu, Jiang, Xiao, Middleware '12)

- Source: [paper PDF](https://ranger.uta.edu/~jiang/publication/Conferences/2012/2012-USENIX-MIDDLEWARE-Yinjin%20Fu,%20Hong%20Jiang,%20and%20Nong%20Xiao,%20%20A%20Scalable%20Inline%20Cluster%20Deduplication%20Framework%20for%20Big%20Data%20Protection.pdf).
- What it is: similarity-based stateful super-chunk routing with handprints; no cross-node dedup by design.
- Verdict: different.

### Extreme Binning (Bhagwat et al., MASCOTS '09)

- Source: [HP Labs tech report](https://www.hpl.hp.com/techreports/2009/HPL-2009-10R2.html).
- What it is: routes each file to a bin by its minimum chunk ID; one disk access per file.
- Verdict: different. File backup.

### DEBAR (Yang et al.) and SiLo (Xia et al., ATC '11)

- Source: [SiLo PDF](https://www.usenix.org/legacy/event/atc11/tech/final_files/Xia.pdf).
- What they are: single-system backup index designs using similarity and locality.
- Verdict: different.

### Liquid (Zhao et al., IEEE TPDS 2014)

- Sources: [IEEE Xplore](https://ieeexplore.ieee.org/document/6552826/); [MADSys page](https://madsys.cs.tsinghua.edu.cn/publication/liquid-a-scalable-deduplication-file-system-for-virtual-machine-images/). Full text returned 403 [abstract only].
- What it is: a dedup file system for VM images with fixed-size chunks, fingerprints computed lazily, P2P transfer, on-demand fetching, copy-on-read local disk cache, and instant cloning.
- Verdict: the closest academic system on the read side. Different in that it is a file system with a central metadata server, not a block device under a stock hypervisor; distribution is P2P, not hash-owned; and it makes no k-versus-N capacity claim.

### LBFS (Muthitacharoen et al., SOSP '01)

- What it is: content-defined chunking plus hashes over a network file system with a client cache.
- Verdict: different. File-level, single server.

### Venti (FAST '02) and Fossil

- Sources: [Venti paper](http://doc.cat-v.org/plan_9/4th_edition/papers/venti/); [fossil(4) man page](https://github.com/wangeguo/plan9/blob/master/sys/man/4/fossil), opened.
- What it is: Fossil "is structured as a magnetic disk write buffer optionally backed by a Venti server for archival storage". Ephemeral snapshots stay on the local disk; archival snapshots are pushed to Venti as SHA-1 content-addressed blocks, which are then fetched from Venti on demand.
- Verdict: conceptually the same two-tier structure. Different in that there is one Venti server, no hash placement across nodes, it is a file system not a block device, and the CAS tier is archival rather than primary capacity.

### Foundation (Rhea, Cox, Pesterev, ATC '08)

- Source: [paper PDF](https://www.usenix.org/legacy/events/usenix08/tech/full_papers/rhea/rhea.pdf), opened.
- What it is: nightly VM disk snapshots archived into a Venti-like CAS with a Bloom filter; 21 MB/s archive, 14 MB/s restore.
- Verdict: different. Offline snapshots, not a live block device.

### DeDe (Clements et al., ATC '09)

- Sources: [USENIX page](https://www.usenix.org/conference/usenix-09/decentralized-deduplication-san-cluster-file-systems); [paper PDF](http://www.scs.stanford.edu/~jinyuan/dede.pdf), opened.
- What it is: hosts on a shared SAN (VMFS) hash blocks in-band, log write summaries, and each host independently dedups out-of-band against a shared index with no central coordinator. 80% space reduction on VDI.
- Verdict: close on "hash in-band, dedup out-of-band, per host, decentralized". Different in that the durable tier is a shared SAN, not local disks.

### DEDIS (Paulo and Pereira, Middleware '14; TOS 2016)

- Sources: [Springer chapter](https://link.springer.com/chapter/10.1007/978-3-662-43352-2_5); [ACM TOS](https://dl.acm.org/doi/10.1145/2876509). Both paywalled [not opened].
- What it is, from the abstracts: exact, off-line dedup for VM volumes on a distributed primary storage infrastructure.
- Verdict: probably close on off-line dedup of primary VM volumes; write path and placement unverified.

### Cumulus (Vrable et al., FAST '09)

- Backup to a thin cloud store. Different.

### Meyer and Bolosky, A study of practical deduplication (FAST '11)

- Source: [paper PDF](https://www.usenix.org/legacy/event/fast11/tech/full_papers/Meyer.pdf), opened.
- What it is: 857 Microsoft desktops, 162 TB. "Space reclaimed improves roughly linearly in the log of the number of file systems in a domain." Grouping machines into one domain matters more than chunking choice or chunk size. 8K Rabin reclaims 18 to 20% more than whole-file dedup.
- Verdict: not a system, but the existing measurement of "N per-host domains versus one global domain". A reviewer will ask why the study's capacity result is not already implied by this curve.

### Sun et al., Cluster and single-node analysis of long-term deduplication patterns (ACM TOS 2018)

- Source: [paper PDF](https://www.fsl.cs.sunysb.edu/docs/msst16dedup-study/tos17dedup-study-a13-sun.pdf), opened.
- What it is: the same dataset analysed as 1, 8, 32 and 128-node clusters under Stateless, Extreme Binning, file-type, HYDRAstor, Stateful and Σ-Dedupe routing. All-zero chunks are 23% of the space and skew logical load onto one node.
- Verdict: measurement, not a system. Relevant to placement skew under hash routing.

## 3. VM and container image stores

### DADI (Li et al., ATC '20)

- Sources: [USENIX page](https://www.usenix.org/conference/atc20/presentation/li-huiba); [paper PDF](https://www.usenix.org/system/files/atc20-li-huiba.pdf), opened; [overlaybd](https://github.com/containerd/overlaybd).
- What it is: block-level layered images with a local log-structured writable layer, P2P tree distribution, 10,000 containers cold-started on 1,000 hosts in 4 s. Chunk-level dedup is an optional build-time step, not a global content-addressed store.
- Verdict: different. Read-only layers; the writable layer never enters a shared CAS.

### Nydus / RAFS

- Source: [nydus-snapshotter](https://github.com/containerd/nydus-snapshotter).
- What it is: chunk-level content-addressable image format, cross-image dedup, lazy loading, P2P through Dragonfly.
- Verdict: different. File-level, read-only.

### Slacker (Harter et al., FAST '16)

- Source: [USENIX page](https://www.usenix.org/conference/fast16/technical-sessions/presentation/harter).
- What it is: lazy fetch from a Tintri NFS server; only 6.4% of pulled image data is read.
- Verdict: different. Centralized.

### CernVM-FS

- Source: [Making containers lazy with Docker and CernVM-FS](https://cds.cern.ch/record/2838138/files/Hardi_2018_J._Phys.__Conf._Ser._1085_032019.pdf).
- What it is: content-addressed, lazy over HTTP, file-level, read-only. File dedup saves 80% of files and 70% of bytes versus layer dedup.
- Verdict: different.

### VMware Instant Clone, KubeVirt containerdisk, Firecracker plus Dragonfly, OpenStack Cinder dedup backends

- [not opened]. All share a read-only base; none put the write path into a content-addressed store.

## 4. Framing: per-host dedup versus cross-host content addressing

No paper measures fleet capacity k versus N copies for VM block storage. The nearest evidence is Meyer and Bolosky's domain-size curve, Sun et al. 2018, the vSAN OSA-to-ESA documentation, and a [Cohesity vendor blog](https://www.cohesity.com/blogs/global-deduplication-matters/) that draws the picture without measuring it. The framing is open. The measurement is what the study would contribute.

## 5. Two-tier log then content-addressed store, single host

- [OpenZFS 2.3 fast dedup](https://klarasystems.com/articles/introducing-openzfs-fast-dedup/), [design discussion #15896](https://github.com/openzfs/zfs/discussions/15896): the DDT log journals dedup-table updates and flushes them in batches. Dedup itself is still inline on write. Single host.
- [dm-vdo design](https://docs.kernel.org/admin-guide/device-mapper/vdo-design.html): a write is acknowledged after physical block allocation, then dedup and compression run asynchronously against the UDS fingerprint index. Single host. This is the nearest single-host analogue to "ack locally, dedup later".
- [Dmdedup, OLS 2014](https://www.fsl.cs.sunysb.edu/docs/ols-dmdedup/dmdedup-ols14.pdf): device-mapper dedup target with pluggable metadata backends, in-band.
- [iDedup, FAST '12](https://www.usenix.org/system/files/conference/fast12/srinivasan.pdf): inline selective dedup, 2 to 4% latency cost for 60 to 70% of maximum dedup.
- [Nitro, ATC '14](https://www.usenix.org/system/files/conference/atc14/atc14-paper-li_cheng_nitro.pdf): deduplicated, compressed SSD cache in front of primary storage; 53% fewer SSD writes.
- [Windows Server dedup, El-Shimi et al., ATC '12](https://www.usenix.org/system/files/conference/atc12/atc12-final293.pdf): post-process, variable chunking, chunk store, single volume.
- zfs send -D: [deprecated and removed in OpenZFS 2.0](https://github.com/openzfs/zfs/issues/7887). It only deduped within one stream and never consulted the DDT.
- CAFTL (FAST '11, dedup inside an SSD FTL), PDS, Wildani/SDS [not opened].

## The three closest works

1. Datrium DVX and US20170031994A1. The study adds peers as hash owners instead of a shared pool, an open implementation on stock QEMU, and a measured cold-read cost per transport. Reviewer sentence if ignored: "Datrium shipped host-side fingerprinting, a host flash cache and global dedup in 2016, and its patent already lists host-only acknowledgement as an alternative; the paper does not cite it."
2. Nutanix AOS, with vSAN ESA as the second data point. The study adds a write path with no synchronous mirror, quantified as latency gained against durability window lost, and hash placement instead of locality placement with the remote-read penalty measured. Reviewer sentence: "The write path is Nutanix's OpLog minus the replication that makes it durable; the paper must show the cost of that omission, not only its latency benefit."
3. Fossil plus Venti, with DeDe as the decentralized out-of-band variant. The study adds a block device rather than a file system, primary capacity rather than archival, and multi-node ownership. Reviewer sentence: "Write-buffer-then-content-addressed-archive is Plan 9's Fossil and Venti from 2002; the paper presents the two-tier structure as new."

Also cite Liquid (nearest academic VM-image system with a local cache and on-demand fetch), Dong FAST '11 (argues against per-chunk hash placement and must be answered), Meyer and Bolosky (already measured the global-domain gain), and Ceph TiDedup (open-source hash placement with VM image numbers).

## Not opened

Liquid full text, DEDIS, the HyperFlex whitepaper, the Hedvig patent (image PDF), the Datrium vGeek recap, HydraFS, CAFTL, PDS, Wildani/SDS, Pure's dedup scope, and whether Nutanix's Unified Cache is keyed by fingerprint. The current Bible says the cache is per CVM and used for dedup; older editions described a fingerprint-keyed "content cache", which I could not confirm in the current text.
