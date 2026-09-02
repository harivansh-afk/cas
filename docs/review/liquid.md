# Liquid (TPDS '14) read in full, against docs/spec.md

Read 2026-09-02. Full text opened, all ten pages, figures read from rendered pages. Chart values below are read off the axes and are approximate to about 2 units unless the text states them.

## The paper

Xun Zhao, Yang Zhang, Yongwei Wu, Kang Chen, Jinlei Jiang, Keqin Li. "Liquid: A Scalable Deduplication File System for Virtual Machine Images." IEEE Transactions on Parallel and Distributed Systems, vol. 25, no. 5, May 2014, pp. 1257-1266. DOI 10.1109/TPDS.2013.173. Received 9 Mar 2013, published online 2 July 2013. Tsinghua (MADSys group) and SUNY New Paltz.

- IEEE Xplore (paywalled): https://ieeexplore.ieee.org/document/6552826
- Open PDF from the authors' group page: https://madsys.cs.tsinghua.edu.cn/publication/liquid-a-scalable-deduplication-file-system-for-virtual-machine-images/TPDS2014-zhao.pdf
- Group page: https://madsys.cs.tsinghua.edu.cn/publication/liquid-a-scalable-deduplication-file-system-for-virtual-machine-images/
- Semantic Scholar: https://www.semanticscholar.org/paper/b41dfa1a54af49a5c5519487d1dfcee29415def3 (67 citations, 7 marked influential, as of today)
- Local copy of the PDF and extracted text: scratchpad only; not committed.

Abstract, last two sentences: "It also provides a comprehensive set of storage features including instant cloning for VM images, on-demand fetching through a network, and caching with local disks by copy-on-read techniques. Experiments show that Liquid's features perform well and introduce minor performance overhead."

## 1. Liquid's design

### Components

Three tiers, each "a commodity Linux machine running a user-level service process" (3.2):

- One meta server with a hot-backup shadow. It holds "file system namespace, fingerprint of data blocks in VM images, mapping from fingerprints to data servers, and reference count for each data block." Every metadata mutation is applied to both. On primary failure the shadow "will take over and operate in read-only mode until an administrator sets up a shadow meta server for it."
- Data servers, "organized in a distributed hash table (DHT) fashion, and governed by the meta server. Each data server is assigned a range in the fingerprint space by the meta server." The meta server heartbeats them round-robin, plus an on-demand probe when any client or server reports a connection error, and issues migration or re-replication instructions.
- Clients, one per VM host. "A Liquid client provides a POSIX compatible file system interface via the FUSE toolkit." The client does the deduplication, the P2P sharing, and cloning.

So placement of the authoritative copy is by hash: the fingerprint space is range-partitioned across data servers, with the map held centrally. P2P is a second, opportunistic tier among client caches on top of that.

### Data model

- Fixed-size blocks. Section 3.3.1 argues from 4 KB filesystem alignment, cites Jin and Miller [17] for fixed chunking on VM images, and adds "since OS and software application data are mostly read-only, they will not be modified once written into a VM image."
- Block size is a compile-time parameter. "it is advised to use a multiplication of 4 KB between 256 KB and 1 MB to achieve good balance between IO performance and deduplication ratio." The evaluation uses 256 KB as "moderate".
- Hash: not stated. Section 3.3.3 says "MD5 and SHA-1 are two cryptography hash functions frequently used for this purpose" and cites a CUDA MD5 note [18] for GPU offload. Read this as MD5 or SHA-1, unspecified.
- Image representation: "Liquid represents a VM image via a sequence of fingerprints which refer to the data blocks inside the VM image." That sequence is "a meta data file containing references to data blocks" stored on the meta server's local filesystem "as individual files, and organized in a conventional file system tree." Since blocks are fixed size, offset to fingerprint is index arithmetic; there is no tree.
- Modified-but-unhashed blocks carry a "private fingerprint": "assigned a randomly generated private fingerprint instead of calculating a new fingerprint on-the-fly ... differs from normal fingerprint only by a bit flag, and part of it is generated from an increasing globally unique number."

### Local block store on each client and data server (3.3.4)

Blocks are grouped by fingerprint. Per group: an extent file (block contents), an index file (fingerprint to offset plus reference count), and a bitmap file (slot valid). Index and bitmap are loaded into memory: "with 256 KB data block size, and 1 TB size of unique data blocks, we will only have an index file size of 80 MB, and a bitmap file size of 512 KB." Lookup is O(1) by hashing. Delete flips a bitmap bit; insert reuses a free slot from a free list or appends.

### Write path (3.3.3, 4.3)

Two memory caches in the client. A shared cache, read-only, LRU, shared across all open images. A private cache holding modified blocks. On write: "When a data block is modified, it is ejected from the shared cache if present, added to the private cache, and assigned a randomly generated private fingerprint." Hashing is deferred: "Liquid delays fingerprint calculation for recently modified data blocks, runs deduplication lazily only when it is necessary." The trigger is "when a hypervisor issues a POSIX flush() request, or the private cache becomes full and chooses to eject it based upon the LRU policy. Only then will the modified data block's fingerprint be calculated."

Ejection is batched to a queue consumed by fingerprint threads (four recommended); they report "linear speedup by increasing the number of fingerprint calculation threads, until the memory bandwidth is reached." Recommended sizes: "256 MB of shared cache and 256 MB of private cache would be sufficient for most cases."

When the bytes reach disk: "Liquid caches these data blocks in memory instead of immediately writing to local disk, in case subsequent writings are issued to these data blocks. New data blocks are written to disk in batch when memory cache becomes full." (4.3)

When the bytes reach another host: "After the shutting down of VMs, the client side uploads modified metadata to meta server, and pushes new data blocks to data servers, to make sure that the other client nodes can access the latest version of image files." (3.2) So dedup against the fleet, and cross-host durability, happen at VM shutdown, not during the run.

Acknowledgment: the paper never says when a guest write is acknowledged relative to disk. The write lands in the private cache in memory. FUSE flush() fires on close of a file descriptor, not on the guest's FLUSH, and the paper does not say fsync is honored or that flush() waits for the disk. There is no durability contract stated anywhere in the paper, and no crash test.

### Read path (3.3.3, 3.5.2, 3.5.3)

Order: shared cache in memory, then local disk cache (copy-on-read), then peers, then data servers.

Peer lookup uses Bloom filters. "Each client node or a data server is a valid data block provider, and publishes a Bloom filter where the fingerprints of all their data blocks are compacted into." Sizing: "256 KB block size, 40 GB unique data blocks, 4 hash functions in Bloom filter, and a constraint of less than 1 percent false positive rate ... at least around 1,724,000 bits, which is about 210 KB in size. In practice, we choose a Bloom filter size of 256 KB." Each client keeps connections to a peer set tracked by the meta server and "periodically updates its copy of peer clients' Bloom filters." On a miss it "checks existence of the fingerprint among peer clients' Bloom filters in a random order, and tries to fetch the data block from a peer if its Bloom filter contains the requested fingerprint. If the data block is not found among peers, the client will go back to fetch the data block from data servers." Random order is the load-balancing mechanism.

Copy-on-read: fetched blocks are written to the local disk cache, so every block a VM ever reads becomes local and the client becomes a provider for it.

Miss cost, stated and not measured: "a cache miss will result in an expensive RPC call for fetching the data block being requested. This incurs several times longer IO delay compared with local disk IO. However, this problem will not impair IO performance greatly, since only the first access to such data blocks will affect IO performance."

No prefetch of any kind.

### Cloning (3.6)

"Simply by copying the meta data file and updating reference counting in data block storage, we could achieve cloning of a VM image ... the cloned VM image is by nature a copy-on-write product ... VM images could be cloned in several milliseconds in the users' view."

### Single writer and migration (3.1, 3.4)

Assumption 1: "A disk image will only be attached to at most one running VM at any given time." Enforced by an edit token held at the meta server and moved to the client that modifies the image, returned after push-back. Migration is never described as an operation and never measured. What the design gives is: stop the VM, push modified blocks to data servers and metadata to the meta server, return the token, open on another client. Live migration is not addressed. The introduction names migration only as motivation for shared access.

### Garbage collection (3.8)

Two mechanisms. Clients keep a reference count per block in their local store; zero-count blocks stay until "the client cache is nearly full," then the extent file is compacted. Data servers keep no counts. "The reference counting of all data blocks is maintained by the meta server, and it periodically issues garbage collection requests to data servers. Based on the data server's fingerprint range, the meta server will generate a Bloom filter containing all the valid data blocks inside the range. The Bloom filter is then randomly sent to one of the replicas, and an offline garbage collection is executed based on data block membership in the Bloom filter." Bloom false positives keep garbage; they never free a live block.

### Fault tolerance (3.7)

"Data blocks are stored in two replicas." Meta server detects a data server failure and orders re-replication. Planned decommission copies extent files and merges them at the destination. Client caches count as extra replicas: "Even if all data servers crash, those blocks are still available through the P2P block sharing protocol." Meta server has the hot shadow described above.

### What the hypervisor sees

A raw-format image file on a FUSE mount. The hypervisor is unmodified and unnamed (2.1 discusses Xen, KVM, VirtualBox; the VM in 4.3 runs Ubuntu 10.10 with 1 vCPU and 2 GB, which reads as KVM). Reads under FUSE go through the host page cache, and the paper relies on it: "Read performance sees weaker impact compared to that of write, because the data being accessed is likely to be cached by OS."

## 2. Every number

Testbed (4.1): 8 blades, 1 Gb Ethernet, each "4 Xeon X5660 CPUs, 24 GB DDR3 memory, and a 500 GB hard drive (Hitachi HDS721050CLA362)", Ubuntu 11.04, kernel 2.6.38. Guest: 50 GB image, ext4, 2 GB RAM, 1 vCPU, Ubuntu 10.10, kernel 2.6.35. Kernel compile is 3.0.4.

Design constants: index 80 MB per TB of unique data at 256 KB blocks (20 bytes per entry; 5 GB per TB at 4 KB by the same constant). Bitmap 512 KB per TB. Bloom filter 256 KB per provider at 40 GB unique. Caches 256 MB plus 256 MB. Four hash threads. Two replicas. Clone in "several milliseconds".

Fig. 4, read throughput from the client's local cache by block size, single HDD (MB/s): 4 KB about 4; 8 KB 7; 16 KB 11; 32 KB 18; 64 KB 31; 128 KB 48; 256 KB 62; 512 KB 68; 1 MB 72; 2 MB to 16 MB 73 to 74. Text: "stabilizes after it reaches 256 KB." This is a seek-bound HDD curve and is the basis for the 256 KB recommendation.

Fig. 5 and Table 1, deduplication ratio (percent of bytes removed) on 183 images totaling 2.31 TB (Windows 75, Ubuntu 32, RedHat 22, Fedora 21, CentOS 21, openSUSE 12; applications installed "randomly"): 1 KB about 80; 2 KB 78; 4 KB 77; 8 KB 71.5; 16 KB 69; 32 KB 66.5; 64 KB 64; 128 KB 62; 256 KB 59; 512 KB 57; 1 MB 56; 2 MB 55; 4 MB 53.5; 8 MB 50; 16 MB 48. Text: "For data block size larger than 4 KB, the deduplication ratio drops quickly." Related work then says "our Liquid has reduced storage consumption by 44 percent at 512 KB data block size", which disagrees with the 57 percent the figure shows at 512 KB; the paper does not reconcile the two.

Fig. 6, Bonnie++ in the guest (MB/s, write / read): native 108 / 142; raw 97 / 102; qcow2 40 / 90; Liquid 2 MB 43 / 83; 1 MB 44 / 120; 512 KB 59 / 129; 256 KB 59 / 123; 128 KB 44 / 97; 64 KB 40 / 91; 32 KB 39 / 90; 16 KB 32 / 81. At the recommended 256 KB, write is 39 percent below raw and read is 21 percent above raw (page cache).

Fig. 7, PostMark in the guest (transactions/s): raw 190; qcow2 75; Liquid 2 MB 57; 1 MB 103; 512 KB 112; 256 KB 182; 128 KB 152; 64 KB 150; 32 KB 140; 16 KB 135.

Fig. 8, normalized time (1.0 is the longest sample), boot / untar / build: native (no boot) 0.55 / 0.77; raw 0.28 / 0.81 / 0.87; qcow2 0.35 / 0.94 / 0.94; Liquid 2 MB 0.40 / 0.82 / 0.89; 1 MB 0.41 / 0.83 / 0.89; 512 KB 0.45 / 0.82 / 0.89; 256 KB 0.46 / 0.82 / 0.90; 128 KB 0.53 / 0.83 / 0.90; 64 KB 0.70 / 0.85 / 0.90; 32 KB 0.93 / 0.86 / 0.90; 16 KB boot bar not visible, untar 0.88, build 0.92. Boot on Liquid at 256 KB is 1.6x raw; untar and build beat qcow2 and trail raw by a few percent.

Fig. 9, time to move an 8 GB fresh Ubuntu 10.10 image from one node to the other seven, 256 KB blocks (seconds): scp about 730; NFS 510; BitTorrent 95; Liquid 35. The Liquid advantage over BitTorrent is attributed to not moving duplicate (mostly zero) blocks.

Fig. 10, boot with on-demand fetching, by block size (seconds). Grey bar is full download then boot, with the download portion in black at about 37 s for every size, so the local-cache boot alone is about 21 to 29 s. Hatched bar is on-demand fetch during boot: 2 MB 37; 1 MB 45; 512 KB 50; 256 KB 88; 128 KB 118. Against the cached boot portion, on-demand is about 1.7x at 2 MB and about 4x at 128 KB. Text: "VM booting with on-demand fetching takes several times longer duration ... the whole VM boot time (the downloading image time and the VM boot time) has been shortened while the data block size is between 512 k and 2 M."

Claims with no figure behind them: "the I/O performance loss is just less than 10 percent" (related work, versus GFS/HDFS style replication). Figs. 6 and 7 do not support it for writes.

Not reported anywhere: any latency, any percentile, cache hit rate, per-miss fetch time, network bytes for on-demand boot, CPU cost of hashing, write amplification, time from write to durable, cluster larger than 8 nodes.

## 3. Diff against the spec

The spec's row on page 06 reads: "fingerprint-keyed VM image filesystem, P2P fetch across hosts, copy-on-read local cache | block device under a stock hypervisor instead of a filesystem; owner by hash instead of P2P; full text not yet read".

**"fingerprint-keyed VM image filesystem, P2P fetch across hosts, copy-on-read local cache."** Accurate.

**"block device under a stock hypervisor instead of a filesystem."** Half right. Liquid is a FUSE filesystem exporting a raw image file, so "block device instead of a FUSE file" is a real difference. "Stock hypervisor" is not: Liquid also ran an unmodified hypervisor. Rewrite as "vhost-user block device instead of a raw file on FUSE, with the host page cache bypassed instead of relied on."

**"owner by hash instead of P2P."** Misleading. Liquid does own by hash. Data servers are "organized in a distributed hash table (DHT) fashion" with each "assigned a range in the fingerprint space by the meta server," and every new block is pushed to its range owner. P2P among clients is a cache tier layered on top. The actual differences are: the spec has no separate data-server tier and no meta server, since hosts are the owners and rendezvous hashing replaces the central fingerprint-to-server map; the spec has no opportunistic peer tier, so a cold read goes to the one owner instead of probing Bloom filters in random order; and the spec's HAS is exact where Liquid's Bloom filter has a 1 percent false-positive budget. Rewrite as "hosts are the owners, placed by rendezvous hashing with no meta server or data-server tier; no P2P cache tier; exact HAS instead of Bloom filters."

**"full text not yet read."** Remove.

### The novelty sentence

Page 06 says: "No prior system is a local-only write log with no network on the write path, a fleet-wide hash-placed chunk store, and remote cold reads under a stock hypervisor."

Liquid has no network on the write path (writes sit in a client memory cache and are pushed at VM shutdown), a fleet-wide hash-placed store (range-partitioned data servers), and remote cold reads under a stock hypervisor (on-demand fetch through FUSE). Read literally, Liquid satisfies all three clauses. What it lacks is the word "log": Liquid's write buffer is 256 MB of volatile memory with no stated durability, no sequence numbers, no crash recovery, and cross-host durability only at shutdown. The sentence should hinge on that and on the block device. Suggested: "No prior system puts a durable, sequence-numbered local log with a stated durability contract in front of a fleet-wide hash-placed chunk store, under a stock hypervisor's block device, and measures the remote cold read per transport. Liquid (TPDS '14) is the closest shape and is the comparison on every point below."

### What Liquid already did that the spec presents as new or does not mention

| Spec item | Liquid | What the spec should say |
|---|---|---|
| Hot path hashes nothing; compaction hashes later (page 01) | Same idea. "Liquid delays fingerprint calculation for recently modified data blocks, runs deduplication lazily only when it is necessary." Private fingerprints stand in until eviction or flush(). | Cite Liquid for deferred hashing. The difference is that the spec's buffer is an fdatasync'd log on NVMe with a watermark, and Liquid's is memory. |
| Settle window absorbs rewrites before chunking | Liquid's private-cache LRU does the same job. "avoids repeated invalid fingerprint calculation." No time parameter, not measured. | Cite; note the spec parameterizes and measures it. |
| Copy-on-read cache | Liquid caches every fetched block on local disk, so after first boot everything is local. The spec has only a memory chunk cache; fetched chunks a host does not own are never persisted, so in partitioned mode they are refetched after memory eviction, forever. | Either add an on-disk cache tier for non-owned chunks or state explicitly that the spec omits it and that the "residual cost of one copy per chunk" on page 04 is measured without one. Liquid did this in 2014 and the omission will be the first question. |
| P2P fetch | Liquid's Bloom-filter peer tier. | The spec has none and says why (one owner is hot for a shared chunk). Say that in the row. |
| Per-image manifest, 32 bytes per chunk | Liquid: "a sequence of fingerprints", a metadata file per image. | Cite. |
| Provisioning cost is the size of the manifest (page 03) | Liquid cloning: copy the metadata file, adjust refcounts, "several milliseconds". Fig. 9 is a provisioning-bytes experiment against scp and NFS, the same baseline the spec uses. | Cite both. The spec's number is manifest bytes; Liquid reported seconds. |
| Migration moves manifest plus staging tail | Liquid moves everything modified to data servers at shutdown, then metadata; equivalent to compaction plus manifest hand-off, with no live path and no measurement. | Say Liquid does it only at shutdown and never measured it. |
| Prefetch (page 04) | None in Liquid. | The spec is new here relative to Liquid. Fig. 10 is the unmitigated penalty to cite. |
| Fixed 4 KB chunks justified by alignment and Jin and Miller (page 00) | Liquid 3.3.1 makes the identical argument with the same citation. | Cite Liquid alongside Jin and Miller. |
| Index bytes per TB as the chunk-size constant (page 02) | Liquid: 80 MB per TB at 256 KB, 20 bytes per entry. | Cite as a prior value of the same constant; the spec's 40 bytes per entry at 4 KB extrapolates to 10 GB per TB, Liquid's constant to 5 GB. |
| No reference counts; LIVE set mark-sweep; leak between sweeps (page 01) | Liquid's data-server GC is exactly this: meta server sends a Bloom filter of the live set, the owner sweeps offline. Liquid's client store does use refcounts. | Cite the data-server side as prior art for LIVE. The spec's exact hash list versus Liquid's Bloom filter is the difference. |
| k replicas as a parameter | Liquid fixes two replicas. | Fine; say so. |
| Single writer per image | Liquid's edit token, Assumption 1. | Cite. |
| Local store as extents with an in-memory index | Liquid 3.3.4: extent, index, bitmap files, free-list slot reuse. | Cite; spec appends and hole-punches instead of reusing slots. |
| Unmodified hypervisor | Liquid too. | Drop "stock hypervisor" as a differentiator against Liquid. |
| Compactor CPU | Liquid: "fingerprint calculation is CPU intensive, and would probably contend with hypervisors"; linear speedup to memory bandwidth with threads. | The spec measures compactor disk interference but never lists compactor CPU per GB as a metric. Add it. |

## 4. What the spec should cite or reproduce from Liquid

- **The on-demand boot penalty, Fig. 10.** 1.7x to 4x over a cached boot depending on block size, on 1 GbE and a single HDD, with no prefetch. This belongs in the page 06 remote-fetch table as a row: "boot 1.7x to 4x of cached at 2 MB to 128 KB blocks; miss cost stated as 'several times longer IO delay compared with local disk IO' and not measured; no prefetch." It is the direct ancestor of the spec's partitioned boot storm with and without profile prefetch, and it strengthens the "nobody measured the per-read cost" claim because Liquid explicitly describes the cost and declines to measure it.
- **Provisioning against scp, Fig. 9.** 8 GB to seven nodes: scp 730 s, NFS 510 s, BitTorrent 95 s, Liquid 35 s. Same baseline as page 03. The spec's provisioning moves manifest only; Liquid's still moved every unique block to every node.
- **Dedup ratio versus block size, Fig. 5.** 77 percent at 4 KB to 59 percent at 256 KB on 2.31 TB of mixed images. The spec's part 1 chunk-size arms produce the same curve on a Linux-only fleet at 4 KB and 16 KB. Cite as the prior curve, with the caveat that 41 percent of Liquid's corpus is Windows.
- **Write overhead at the recommended block size, Fig. 6.** Liquid at 256 KB wrote at 61 percent of raw. The spec's hypothesis 1 bound of guest p99 within 20 percent of raw is much tighter than what Liquid achieved on throughput; worth stating as the bar Liquid did not clear.
- **Fig. 4 as an HDD artifact.** Liquid's 256 KB choice comes from a seek-bound read curve (4 MB/s at 4 KB). On NVMe that curve does not exist, which is the spec's justification for revisiting 4 KB and 16 KB. One sentence on page 02 closes this.
- **Index constant.** 20 bytes per entry, 80 MB per TB at 256 KB.
- **Hashing throughput.** Liquid reports linear speedup with threads up to memory bandwidth; the spec should report BLAKE3 GB/s per core in the compactor and the cores it consumes at the sustainable ingest rate.

Liquid did not measure: cache hit rate, per-read remote fetch time, the length of the deferral window, bytes moved during an on-demand boot, or any latency percentile. None of these is available to cite.

## 5. What Liquid got wrong or left open

- **No durability contract.** Writes live in a 256 MB memory cache, hashed on FUSE flush() or LRU eviction, written to disk "in batch when memory cache becomes full," and shipped to data servers at VM shutdown. A host crash loses the private cache; a host loss loses every write since the VM booted. The paper never mentions fsync, crash recovery, or what a guest FLUSH means. The spec's local class with a measured (D, E] window, fleet class, the watermark, and the kill -9 replay test are the direct answer, and the spec can say Liquid left the whole question open.
- **Block size chosen for HDD.** 256 KB to 1 MB costs 18 to 21 points of deduplication on their own corpus (Fig. 5) to buy HDD throughput (Fig. 4). With 256 KB blocks, a 4 KB guest write re-hashes and re-stores 256 KB, a 64x write amplification the paper never measures. The spec measures write amplification and runs 4 KB on NVMe.
- **A central meta server.** Every image open, every clone (which updates a refcount per block on the meta server, 160 thousand at 256 KB for a 40 GB image and 10 million at 4 KB), and every fingerprint-to-server lookup goes through one process with a hot spare that fails over read-only. The paper claims P2P "solved the bottleneck problem of metadata server" but measures nothing above 8 nodes. Rendezvous hashing with no meta server and no refcounts is the claimable difference.
- **No latency anywhere.** Throughput and wall-clock seconds only. The title says scalable; the largest experiment is 8 blades on 1 GbE.
- **Internal inconsistencies.** "44 percent at 512 KB" against 57 percent in Fig. 5; "I/O performance loss is just less than 10 percent" against a 39 percent write deficit in Fig. 6.
- **Reads leaned on the host page cache.** Stated outright in 4.3. The spec's O_DIRECT and cache=none discipline removes that confound and is worth naming as a correction.
- **Bloom-filter peer probing.** Random-order probes with 1 percent false positives add an unmeasured round trip per false hit, and the peer set and its filters go stale between exchanges. The spec's single known owner has neither problem.
- **Hash unspecified, and MD5 or SHA-1 if taken at face value.** BLAKE3 is the modern choice and the spec says so.
- **No FUSE-passthrough control.** Liquid never separated the cost of FUSE from the cost of deduplication. The spec's R0 versus R3 pair with a passthrough gate (G1) does.
- **Migration never defined or measured.** Only stop, push, reopen.

## Suggested edits to the spec

1. Page 06 nearest-systems row for Liquid: "FUSE file system exporting a raw image; fixed 256 KB to 1 MB blocks; writes buffered in client memory, hashed on flush() or eviction, pushed to range-owning data servers at VM shutdown; Bloom-filter P2P among client caches; copy-on-read disk cache; clone by metadata copy; refcount GC on clients and Bloom-filter mark-sweep on servers; 8 nodes, 1 GbE, HDD | vhost-user block device instead of FUSE; a durable sequence-numbered log with a stated FLUSH contract instead of a volatile cache flushed at shutdown; hosts are the owners by rendezvous hashing, no meta server, no data-server tier, no refcounts; 4 KB and 16 KB chunks on NVMe instead of 256 KB on HDD; prefetch; latency measured per transport."
2. Page 06 remote-fetch table: add the Fig. 10 row above.
3. Page 06 novelty sentence: rewrite as proposed in section 3.
4. Page 01 read path: decide whether fetched non-owned chunks are persisted on local disk. If not, say so and say why, with Liquid's copy-on-read as the alternative.
5. Page 01 compactor and page 02 metrics: add compactor CPU per GB hashed.
6. Page 00 and page 02: cite Liquid 3.3.1 beside Jin and Miller for fixed chunking, and Fig. 5 as the prior dedup-versus-block-size curve.
7. Page 03 provisioning: cite Liquid's clone-by-metadata-copy and Fig. 9.
