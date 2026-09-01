# Prior work on remote chunk fetch inside the VM read path

Date: 2026-09-01. Scope: what prior systems measured about lazily or remotely fetching image data at read time, the observed miss latency, and the mitigations, as background for the cold-read cost of a content-addressed block backend where the chunk lives on the other host.

## Verdict

Nobody has published a microsecond-scale measurement of a content-addressed chunk fetched from a peer node inside a VM block-device read path. The closest per-unit numbers are SnowFlock's 275 us per page over gigabit unicast (memory, 2009), FaaSnap's 13.3 us per lazy page fault from local disk with userfaultfd adding over 128 us when the page is not cached, and Dahlin's 1994 cost model of 1.25 ms for a remote-memory hit. DADI, VMThunder, Nydus, eStargz and Slacker report startup seconds and concede a per-read penalty without quantifying it.

The field's answer is to hide the miss, not to make it fast. Every system that reports removing most of the cold cost does so with a recorded access profile replayed as prefetch. The transport tricks (peer page-cache relay, over-fetch, in-kernel cache) shave the constant but nobody measures the constant.

Everything below was opened unless marked [not opened]. Extracted texts of DADI, Slacker, TiDedup and the FAST '13 restore paper are in the session scratchpad under `pdf/`.

## 1. DADI (Alibaba, USENIX ATC '20)

- Source: [atc20-li-huiba.pdf](https://www.usenix.org/system/files/atc20-li-huiba.pdf).
- Block-level virtual disk (overlayBD). Layers are indexed by LBA range; a read does a range lookup over a merged index, more than 6M lookups per second. Not content-addressed. Chunk dedup is an optional offline step, never on the read path.
- P2P is one tree per layer blob rooted at DADI-Root, which fetches from the registry into a persistent cache. A miss walks up the tree until a parent has the block in cache. Children add received blocks to their own persistent cache. Trees expire after startup.
- Section 5.2: WordPress cold start from the P2P root stays at about 0.7 s from 1 to 32 hosts; pseudo-Slacker goes from 1.5 s to 2.3 s. Trace-based prefetch (blktrace record, fio replay at queue depth 32) removes 95% of the gap between cold and warm startup. Section 5.3: 10,000 containers on 1,000 hosts in about 4 s.
- Uncached random read (Figure 22) peaks near 120K IOPS at queue depth 128, given only as a bar chart. No per-read miss latency anywhere.
- The sentence that matters for this design: "with the tree-structured P2P data transfer, hosts effectively read from their parents' page cache, and this is faster than reading from their local disks."

## 2. Nydus, eStargz, EROFS over fscache

### Nydus / Dragonfly

- Sources: [nydus-design.md](https://github.com/dragonflyoss/nydus/blob/master/docs/nydus-design.md), [prefetch.md](https://github.com/dragonflyoss/nydus/blob/master/docs/prefetch.md), [nydus-fscache.md](https://github.com/dragonflyoss/nydus/blob/master/docs/nydus-fscache.md).
- Chunk size 1 MiB by default (`RAFS_DEFAULT_CHUNK_SIZE` in `storage/src/lib.rs`). A miss is one HTTP range GET per chunk, optionally through the Dragonfly dfdaemon proxy. Blob cache on local disk with no eviction. A prefetch table built from access traces at image build time, or at runtime by the containerd NRI optimizer plugin. Adjacent chunk reads merge into one backend request (`merging_size`).
- Ant Group numbers (2023): WordPress 11.7 s OCI, 5.2 s Nydus FUSE, 4.5 s fscache. 1 GB image with 50 concurrent pulls: OCI 145 s, Nydus plus Dragonfly 65 s. No per-read miss latency published. No formal paper exists.

### EROFS over fscache (Linux 5.19)

- Source: [LWN 894364](https://lwn.net/Articles/894364/), the patch cover letter.
- On a miss the kernel blocks the read, sends a request over `/dev/cachefiles`, the user daemon fetches from the registry and writes into the cache file, the kernel resumes. Hits stay in-kernel. The daemon over-fetches, 1 MB for a 4 KB request.
- fio randread 9.5K IOPS on fscache against 7.6K on FUSE; tar of many small files 0.57 s fscache, 3.2 s FUSE, 1.04 s ext4 (Dragonfly evolution blog). The Alibaba Cloud blog is [not opened], JS-rendered.

### eStargz / stargz-snapshotter

- Sources: [stargz-snapshotter README](https://github.com/containerd/stargz-snapshotter), [estargz.md](https://github.com/containerd/stargz-snapshotter/blob/main/docs/estargz.md), [google/crfs](https://github.com/google/crfs).
- Per-file gzip members, 4 MiB chunk default for large files, a TOC with per-chunk digests, a landmark file that marks the prefetch prefix, then background fetch of the whole layer. Misses are HTTP range GETs through FUSE.
- The README concedes lazy pulling "causes runtime performance penalty because reading files induce remotely downloading contents." A third-party measurement (zmalik.dev, nginx, in-cluster registry): pull 0.088 s FUSE against 3.6 s overlayfs, but readiness 0.271 s against 0.013 s, and PyTorch imports "reach seconds as the FUSE daemon serializes dozens of HTTP Range requests."

### Cross-system comparisons

- Starlight (NSDI '22): [nsdi22-paper-chen_jun_lin.pdf](https://www.usenix.org/system/files/nsdi22-paper-chen_jun_lin.pdf). Startup touches under 1% of files. eStargz becomes slower than plain containerd for postgres at 150 ms RTT or above because on-demand requests round-trip per file and queue at the registry. Starlight pushes files in recorded access order and is 3.0x baseline, 1.9x eStargz.
- SOCI (AWS, arXiv 2607.06868, July 2026): cold per-file access median 62 ms, warm 4.6 ms; FUSE on a warm cache p50 1.85 us; lazy loading loses to full pull above about 80% access density.
- FaaSNet (ATC '21): 512 KB blocks, binary function trees over VMs, 2,500 containers on 1,000 VMs in 8.3 s, 2.8x faster than DADI plus P2P in their setup; on-demand alone gets 2.9x slower from 8 to 128 containers because the registry stays the bottleneck.
- No published head-to-head Nydus against eStargz read-latency benchmark found.

## 3. Slacker (FAST '16)

- Source: [fast16-papers-harter.pdf](https://www.usenix.org/system/files/conference/fast16/fast16-papers-harter.pdf).
- Section 4: pulling is 76% of container start time; only 6.4% of pulled data is read.
- Design: one NFS file per image on a Tintri VMstore, mounted as a loopback block device, with server-side block-level COW snapshots and clones. Blocks are fetched lazily as read.
- Section 6.1: pull is 72x faster and push 153x, "but the run phase is 17% slower (the AUFS pull phase warms the cache for the run phase)". Section 6.2: long-running workloads converge to AUFS throughput and start serving 3 to 19x sooner. Section 6.3: clones share cache only with their loopback kernel patch. No per-block miss latency.

## 4. Firecracker snapshots, REAP, FaaSnap

- REAP (ASPLOS '21): [arXiv 2101.09355](https://arxiv.org/pdf/2101.09355). Thousands of page faults per invocation, contiguity of 2 to 3 pages so readahead is useless, effective SSD bandwidth 43 MB/s. Working sets 8 to 99 MB, about 9% of the footprint, stable across invocations. Record once, then install the working set with one contiguous read: helloworld 232 ms to 60 ms, 3.7x average on FunctionBench. Recording costs 28% once. No remote-storage measurement; the paper says it "can support" remote.
- FaaSnap (EuroSys '22): [faasnap-eurosys22.pdf](https://sysnet.ucsd.edu/~voelker/pubs/faasnap-eurosys22.pdf). Per-fault cost (bpftrace on `kvm_mmu_page_fault`): warm 2.5 us, page-cached 3.7 us, disk-backed 13.3 us average with 9% over 32 us, REAP 6.7 us. Userspace uffd faults cost 8 to 64 us cached and over 128 us from disk. A loader thread prefetches the recorded set into the page cache concurrently, turning major faults into minor ones. On remote EBS FaaSnap is 28% slower than local NVMe but still 2.06x Firecracker.
- Firecracker uffd docs: [handling-page-faults-on-snapshot-resume.md](https://github.com/firecracker-microvm/firecracker/blob/main/docs/snapshotting/handling-page-faults-on-snapshot-resume.md). A separate handler process does `UFFDIO_COPY` from a mapped snapshot; if the handler dies the VM hangs at the next fault. No numbers. The NSDI '20 Firecracker paper has no snapshot numbers.
- SnowFlock (EuroSys '09): [lagar-cavilla-eurosys-snowflock-2009.pdf](https://www.cs.cmu.edu/~satya/docdir/lagar-cavilla-eurosys-snowflock-2009.pdf). Memtap copy-on-access fetch averages 275 us per page over gigabit unicast TCP, 82% of it in the network stack. Avoidance heuristics cut transfer to 40 MB of a 1 GB footprint. This is the closest published "one remote page inside a running VM" number.

## 5. VM-image P2P and lazy boot

- VMTorrent (CoNEXT '12): [p289.pdf](https://conferences.sigcomm.org/co-next/2012/eproceedings/conext/p289.pdf). Demand misses get absolute priority over prefetch. Prefetch order comes from a profile of (first-touch time, piece index, fraction of runs) recorded over one or many runs, under 512 KB. Images of 3.9 to 4.3 GB, 6 to 10% touched. At 100 Mbps with 100 clients, centralized on-demand runs at 40 to 70x the memory-cached time, P2P with profile at 2 to 3x. Fedora with a profile beats local disk.
- VMThunder (TPDS '14): IEEE bronze OA PDF opened. Transfer on demand, cache on read, relay from each host's read cache to children in a static tree, per-VM local COW. 160 VMs boot in about 17 s against 16 s local. DADI names it as the model for its tree. No cache-hit-rate figure.
- vTube (SoCC '13): [socc2013vtube.pdf](https://roxanageambasu.github.io/publications/socc2013vtube.pdf). 4 KB chunks clustered by 2 s access interval across traces. Miss rate under 2% over a 7.2 Mbps, 120 ms link, at the cost of 25 to 175 s initial buffering.

## 6. JuiceFS, Alluxio, object-store first-byte

- JuiceFS: [cache guide](https://juicefs.com/docs/community/guide/cache/), [internals](https://juicefs.com/docs/community/internals/), [read performance blog](https://juicefs.com/en/blog/engineering/optimize-read-performance). 64 MiB chunks, 4 MiB blocks. Read hierarchy is page cache, client buffer, local disk cache, object store. A miss to the object store is "usually greater than 10 ms" with a "fixed overhead of 10-30 ms" per object API call; a single-connection 4 MiB GET averages 98.85 ms. Readahead hits are under 200 us at p99. Their rule of thumb: "distributed cache is 1-2 ms; for local cache, it's 0.2-0.5 ms." `--prefetch` defaults to one whole block, which they warn amplifies sparse random reads 1 to 3x.
- Alluxio local cache in Presto: [prestodb blog](https://prestodb.io/blog/2020/06/16/alluxio-datacaching/). 1 MB pages on local SSD. A 600-node production run cut P50 query latency 33%, P95 48%, remote bytes 57%. No per-read numbers.
- S3 Express One Zone: [s3-express-performance](https://docs.aws.amazon.com/AmazonS3/latest/userguide/s3-express-performance.html), "single-digit millisecond" first byte. mountpoint-s3 keeps a prefetch window up to 2 GiB per file handle.

## 7. TiDedup (USENIX ATC '23)

- Source: [atc23-oh.pdf](https://www.usenix.org/system/files/atc23-oh.pdf), and [Ceph dedup dev docs](https://docs.ceph.com/en/latest/dev/deduplication/).
- Base tier holds metadata objects with a `chunk_map`; chunk tier holds chunk objects whose OID is the fingerprint, placed by CRUSH. They call this double hashing: no fingerprint index. FastCDC with 16 KB average, SHA1. Hot objects are never deduped; cold objects are deduped only if their intra-object duplicate ratio is at least 30%.
- Read of a MISSING chunk: the OSD calls `tier_promote` to copy the chunk back into the base tier before answering, one extra OSD-to-OSD hop, and the object flips back to hot. Section 6: "serving cold data entails forwarding overheads between base and chunk tier."
- No isolated read-latency or read-amplification figure. YCSB-A throughput is "not degraded significantly" against no dedup; the worst case with crawler, scrub and 4 of 36 OSDs down is 25% below no dedup. Testbed: 6 nodes, 100 GbE, QLC NVMe, replication 2, Reef.
- The ICDCS '18 predecessor ("Design of Global Data Deduplication for a Scale-out Distributed Storage System", Oh et al.) is [not opened], Xplore only.

## 8. Ceph RBD low-queue-depth latency

- [Crimson vs Classic, Aug 2025](https://ceph.io/en/news/blog/2025/crimson-seastore-vs-classic/): 4K random read at iodepth 1 per job, Classic 0.29 ms average, Seastore 0.25 ms. Single node, 8 NVMe, so a floor rather than a network number.
- [Reef RBD performance](https://ceph.io/en/news/blog/2023/reef-freeze-rbd-performance/): 100 GbE, EPYC 7742, QD1 4K sync write 0.42 ms average, p99 0.46 ms, p99.9 0.73 ms. No QD1 read row.
- [42on QD1 4K](https://42on.com/rbd-latency-with-qd1-bs4k/): NVMe, 100 GbE, v15, QD1 write 0.73 ms. [Croit](https://croit.io/blog/ceph-performance-test-and-optimization): Nautilus, NVMe, 100 GbE, QD1 write 0.42 ms after C-state tuning. Proxmox forum, SATA SSD, 10 GbE: QD1 4K read 0.64 ms.
- Take 0.3 to 0.7 ms as the always-remote 4K read band on modern hardware. Unmeasured by me.

## 9. Content-addressed and cooperative caches

- CLB (VEE '17, "Content Look-Aside Buffer for Redundancy-Free Virtual Disk I/O and Caching", Yang, Liu, Cheng, PKU, DOI 10.1145/3050748.3050762): [not opened], ACM 403 and no open copy. Abstract: persistent fingerprints on virtual-disk blocks, redundant reads served from other guests' page cache on the same host, 4.1x sequential and 26.2x random read on duplicate data, 94.9 to 98.5% of boot reads eliminated. Single host only.
- Satori (ATC '09): [milos.pdf](https://www.usenix.org/legacy/event/usenix09/tech/full_papers/milos/milos.pdf). Hashes blocks as they are read in blktap and shares pages by content across VMs on one host. VM2 reads at 111 MB/s from VM1's page cache against 4.96 MB/s from disk. Worst-case sequential-read overhead 34.8%.
- Nitro (ATC '14): [atc14-paper-li_cheng_1.pdf](https://www.usenix.org/system/files/conference/atc14/atc14-paper-li_cheng_1.pdf). Dedup plus LZ4 in an SSD cache packed into 2 MB write-evict units. Hit ratio up 14 to 25%, read response time down 41 to 55%.
- CacheDedup (FAST '16): usenix PDF opened. D-LRU and D-ARC. Fingerprinting adds 10 to 20 us per I/O on cold random reads. Latency down 42 to 51% on WebVM and Mail traces. No cross-host sharing.
- Dahlin et al. (OSDI '94): [dahlin.a](https://www.usenix.org/legacy/publications/library/proceedings/osdi/full_papers/dahlin.a). Cost model on 155 Mb/s ATM: 8 KB block 250 us from local memory, 1,250 us from remote client memory over 3 hops, 15 ms from disk. N-Chance forwarding gives 1.73x, within 10% of ideal.
- Liquid (TPDS '14, Zhao et al., DOI 10.1109/TPDS.2013.173): [not opened], IEEE only. Abstract: fingerprint-keyed dedup file system for VM images, P2P chunk transfer across hosts, on-demand fetch, copy-on-read local cache, "minor performance overhead". This is the closest open design to the proposed one and needs to be acquired.
- Ceph immutable object cache: [docs](https://docs.ceph.com/en/latest/rbd/rbd-persistent-read-only-cache/). Per-host daemon caching parent-snapshot objects on local SSD, keyed by object name, not content hash. No numbers. Nutanix content cache, VMware CBRC, IBM Mirage: [not opened], search budget exhausted.

## 10. Dedup read fragmentation

- Lillibridge, Eshghi, Bhagwat (FAST '13): [fast13-final124.pdf](https://www.usenix.org/system/files/conference/fast13/fast13-final124.pdf). Speed factor is 1 over containers read per MB. Over 480 backups, containers per MB grows 18x with a 1 GB cache and restore falls to about 8 MB/s. Capping recovers 4 to 8.8x restore speed for 2 to 23% dedup loss; a forward assembly area is 2 to 4x over LRU at equal RAM.
- iDedup (FAST '12): [Srinivasan.pdf](https://www.usenix.org/legacy/events/fast12/tech/full_papers/Srinivasan.pdf). Dedup only runs of blocks at least T sequential on disk. T=1 adds 13% to mean read latency on the Corporate trace, concentrated in the 10% of requests over 2 ms; T=8 stays within 2 to 4% while keeping 60 to 70% of the dedup.
- Kaczmarczyk et al. (SYSTOR '12, HYDRAstor): [slides PDF](https://9livesdata.com/wp-content/uploads/2017/04/AsPresentedOnSYSTOR-1.pdf). Restore of the latest backup drops 12 to 55% after 7 backups. Context-based rewriting of 0.5 to 2.6% of blocks recovers to 93 to 96% of optimal.
- Fu et al. (ATC '14, HAR): [atc14-paper-fu_min.pdf](https://www.usenix.org/system/files/conference/atc14/atc14-paper-fu_min.pdf). The latest Linux backup restores 21x slower than the first. Rewriting 0.45 to 1.99% of data gives 2.6 to 17x. Splits fragmentation into sparse containers, which a cache cannot fix, and out-of-order containers, which it can.
- RevDedup ([arXiv 1302.0621](https://arxiv.org/abs/1302.0621)): VM image read throughput drops from 606 to 266 MB/s over 12 weekly backups with 128 KB segments; 4 to 32 MB segments hold 1.2 to 1.7 GB/s.
- El-Shimi et al. (ATC '12): [atc12-final293.pdf](https://www.usenix.org/system/files/conference/atc12/atc12-final293.pdf). Measures no read penalty at all. Mitigation is structural: 64 KB chunks in large containers, post-process only.
- Nam et al. CFL papers (HPCC '11, MASCOTS '12), CABdedupe (IPDPS '11), the Proc. IEEE 2016 survey: [not opened], Xplore only. CABdedupe is about omitting unmodified data from WAN transfer, not read locality.
- No paper found that measures dedup turning reads remote in a distributed store. Khan et al. ([arXiv 1803.07722](https://arxiv.org/abs/1803.07722), cluster dedup on Ceph) only notes that small chunk I/Os "still directed over the network" limit the gains. Treat this as not found, not as absent.

## Answers

### (a) Has anyone measured a peer chunk fetch inside a VM block read path at microsecond scale?

No. The in-VM per-unit remote fetch numbers that exist are SnowFlock's 275 us per memory page over 2009 gigabit unicast and FaaSnap's 13.3 us local-disk lazy fault, with uffd adding over 128 us when uncached. Dahlin's 1994 model puts a remote-memory block at 1.25 ms and JuiceFS puts a distributed-cache hit at 1 to 2 ms today. Every block-level image system reports startup seconds and admits a per-read penalty without giving the per-read number. Liquid and CLB are the two content-hash designs closest to this one and neither is openly available. A microsecond measurement of this path over 100GbE or RDMA would be new.

### (b) Consensus mitigations, workload-level against transport-level

Workload-level: record an access profile once and prefetch it. DADI replays a blktrace, REAP installs a working-set file, FaaSnap loads a recorded set concurrently, VMTorrent and vTube order pieces by first-touch time, Nydus and eStargz embed a prefetch list, Starlight streams files in access order. Every one reports removing most of the cold cost; DADI puts it at 95%.

Transport-level: a persistent local cache with copy-on-read (all of them); a tree or P2P relay so a miss hits a peer's page cache instead of the origin (DADI, VMThunder, FaaSNet); over-fetch or request merging to amortise round trips (EROFS 1 MB, Nydus `merging_size`, JuiceFS 4 MiB block); moving the cache into the kernel to skip FUSE (EROFS over fscache).

Placement-level, from the dedup literature: keep hot data whole or local (TiDedup hot and cold tiers, iDedup sequence threshold); rewrite or cap scattered chunks so a read touches fewer containers (FAST '13 capping, HAR, CBR).

Nobody makes the miss itself fast. Everyone hides it.

### (c) Closest three works

1. DADI. A block device under a VM or container, layered read-only image, local persistent cache, tree P2P from peer page caches, trace-based prefetch. Differs in that it is not content-addressed, has no owner-by-hash placement, and uses a separate log-structured writable layer rather than a staging log plus compactor.
2. TiDedup. CDC chunks keyed by fingerprint and placed by CRUSH across nodes, tiered so hot data stays whole. Differs in that it is an object store not a VM block path, promotes on miss instead of caching, has no per-host cache, and reports no latency.
3. Liquid. Fingerprint-keyed VM image chunks, P2P fetch across hosts, copy-on-read local cache. Differs in that it is a file system over VM images, not vhost-user-blk. Unopened, so a citation to acquire before claiming the difference.

Also worth a line: CLB for content-hash reads served from a neighbour's cache on the same host, and Slacker for the plain finding that lazy block fetch under a running container costs 17% on the run phase.
