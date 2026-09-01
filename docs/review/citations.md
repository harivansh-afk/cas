# Citation check for docs/spec.md

Checked 2026-09-01. 20 items: 14 confirmed, 3 partly, 2 wrong, 1 confirmed from the abstract only. Every paper was opened as a PDF or abstract page except where noted. USENIX and ACM HTML pages returned 403 to the fetch tool; PDFs were pulled with curl. Downloaded PDFs and extracted text are in this directory.

## 1. Meyer & Bolosky, FAST '11 — CONFIRMED

Dutch T. Meyer, William J. Bolosky. "A Study of Practical Deduplication." FAST '11.
https://www.usenix.org/legacy/events/fast11/tech/full_papers/Meyer.pdf

Abstract: file system content from 857 desktops at Microsoft over 4 weeks; whole-file vs block-level dedup. Whole-file gets about three quarters of the savings of the most aggressive block-level dedup on live file systems, 87% for backups. Spec claim accurate.

## 2. Jin & Miller, SYSTOR '09 — CONFIRMED

Keren Jin, Ethan L. Miller. "The Effectiveness of Deduplication on Virtual Machine Disk Images." SYSTOR 2009. DOI 10.1145/1534530.1534540.
PDF: https://ssrc.us/media/pubs/082a25b906aa716ca3c2439b8c1889449ecac44c.pdf
ACM: https://dl.acm.org/doi/10.1145/1534530.1534540

Abstract: "fixed-length chunks work well, achieving nearly the same compression rate as variable-length chunks." Conclusion: fixed-size chunking "outperforming variable-sized chunking in some cases." Variable-size chunking used Rabin fingerprints. Spec claim accurate.

## 3. DeDe, ATC '09 — CONFIRMED

Austin T. Clements, Irfan Ahmad, Murali Vilayannur, Jinyuan Li. "Decentralized Deduplication in SAN Cluster File Systems." USENIX ATC '09.
https://www.usenix.org/legacy/events/usenix09/tech/full_papers/clements/clements.pdf

Out-of-band, fixed 4 KB blocks on VMware VMFS. "a real enterprise VDI deployment can expend as much as 80% of its overall storage footprint on duplicate data from VM disk images." "production VDI deployment of 113 Windows XP VMs." Spec claim accurate.

## 4. Jayaram et al., Middleware '11 — CONFIRMED (abstract only)

K. R. Jayaram, Chunyi Peng, Zhe Zhang, Minkyong Kim, Han Chen, Hui Lei. "An empirical analysis of similarity in virtual machine images." Middleware 2011 Industry Track. DOI 10.1145/2090181.2090187.
ACM: https://dl.acm.org/doi/10.1145/2090181.2090187 (403 to the fetch tool)
Abstract: https://research.ibm.com/publications/an-empirical-analysis-of-similarity-in-virtual-machine-images

Abstract: 525 VM images from a production IaaS cloud; similarity within and between images. Spec claim accurate. Full text not opened.

## 5. El-Shimi et al., ATC '12 — CONFIRMED

Ahmed El-Shimi, Ran Kalach, Ankit Kumar, Adi Oltean, Jin Li, Sudipta Sengupta. "Primary Data Deduplication – Large Scale Study and System Design." USENIX ATC '12.
https://www.usenix.org/system/files/conference/atc12/atc12-final293.pdf

15 globally distributed file servers, over 2000 users. Windows Server 2012. "Our system is based on a post-processing approach." Rabin-based variable-size chunking plus their "regression chunking", about 80 KB average. Spec claim accurate.

## 6. DupHunter, ATC '20 — WRONG attribution

DupHunter is a registry dedup system, not the Docker Hub measurement.

System: Nannan Zhao, Hadeel Albahar, Subil Abraham, Keren Chen, Vasily Tarasov, Dimitrios Skourtis, Lukas Rupprecht, Ali Anwar, Ali R. Butt. "DupHunter: Flexible High-Performance Deduplication for Docker Registries." USENIX ATC '20. Up to 6.9x storage reduction.
https://www.usenix.org/system/files/atc20-zhao.pdf

Measurement (DupHunter's reference [72]): Nannan Zhao, Vasily Tarasov, Hadeel Albahar, Ali Anwar, Lukas Rupprecht, Dimitrios Skourtis, Amit S. Warke, Mohamed Mohamed, Ali R. Butt. "Large-Scale Analysis of the Docker Hub Dataset." IEEE CLUSTER 2019. DOI 10.1109/CLUSTER.2019.8891000. About 167 TB uncompressed; about 97% of files across layers are duplicates.
https://ieeexplore.ieee.org/document/8891000/
https://par.nsf.gov/biblio/10167826-large-scale-analysis-docker-hub-dataset

Correction: cite CLUSTER '19 for "file-level redundancy across Docker Hub"; cite DupHunter only for the system.

## 7. iDedup, Dmdedup, dm-vdo — CONFIRMED

iDedup: Kiran Srinivasan, Tim Bisson, Garth Goodson, Kaladhar Voruganti. "iDedup: Latency-aware, inline data deduplication for primary storage." FAST '12.
https://www.usenix.org/legacy/events/fast12/tech/full_papers/Srinivasan.pdf

Dmdedup: Vasily Tarasov, Deepak Jain, Geoff Kuenning, Sonam Mandal, Karthikeyani Palanisami, Philip Shilane, Sagar Trehan, Erez Zadok. "Dmdedup: Device Mapper Target for Data Deduplication." Ottawa Linux Symposium 2014.
https://www.kernel.org/doc/ols/2014/ols2014-tarasov.pdf

dm-vdo: merged for Linux 6.9 on 2024-03-13.
https://www.phoronix.com/news/DM-VDO-Merged-Linux-6.9

## 8. OpenZFS timeline — CONFIRMED, one caveat

- ZFS integrated into Solaris trunk 2005-10-31; OpenSolaris build 27 on 2005-11-16 (https://en.wikipedia.org/wiki/ZFS). Snapshots and clones are part of the original design, but I found no primary dated source listing clones in that first drop. Caveat only.
- Dedup: snv_128, pool version 21, November 2009. https://cuddletech.com/2009/11/first-look-at-zfs-deduplication/
- Block cloning: zfs-2.2.0, released 2023-10-13. https://github.com/openzfs/zfs/releases/tag/zfs-2.2.0
- Fast dedup: zfs-2.3.0, released 2025-01-14 (GitHub API `published_at`; PR #15896). https://github.com/openzfs/zfs/releases/tag/zfs-2.3.0
- Dedup log: `zfs_dedup_log_*` module parameters listed in the 2.3.0 release notes.
- `dedup_table_quota`: pool property, "sets a limit on the on-disk size of the pool's dedup table." https://openzfs.github.io/openzfs-docs/man/master/7/zpoolprops.7.html
- `zpool ddtprune -d days | -p percentage pool`: "prunes older unique entries from the dedup table." https://openzfs.github.io/openzfs-docs/man/master/8/zpool-ddtprune.8.html
- `feature@fast_dedup` is the feature name. https://openzfs.github.io/openzfs-docs/man/master/7/zpool-features.7.html

## 9. TiDedup, ATC '23 — PARTLY

Myoungwon Oh, Sungmin Lee, Samuel Just, Young Jin Yu, Duck-Ho Bae, Sage Weil, Sangyeun Cho, Heon Y. Yeom. "TiDedup: A New Distributed Deduplication Architecture for Ceph." USENIX ATC '23.
https://www.usenix.org/system/files/atc23-oh.pdf

Mechanism matches: a crawler selects objects ("selective cluster-level crawling"), event-driven tiering with content-defined chunking into a chunk pool. Evaluation uses FastCDC, 16 KB average, SHA1. The headline number is "up to 34% data reduction on real-world workloads." Table 2: virtual disks 45/36/27% at 8/16/32K (fixed: 21/12/10%); logs 18.5/16/12.6% (fixed: 5.7/5.4/5.3%).

Correction: write "up to 34%".

## 10. HYDRAstor, FAST '09 — CONFIRMED

Cezary Dubnicki, Leszek Gryz, Lukasz Heldt, Michal Kaczmarczyk, Wojciech Kilian, Przemyslaw Strzelczak, Jerzy Szczepkowski, Cristian Ungureanu, Michal Welnicki. "HYDRAstor: a Scalable Secondary Storage." FAST '09.
https://www.usenix.org/legacy/events/fast09/tech/full_papers/dubnicki/dubnicki.pdf

"a grid of storage nodes built around a distributed hash table"; variable-sized, content-addressed, immutable blocks; "global duplicate elimination." Spec claim accurate. Note it is secondary (backup) storage.

## 11. CLB, VEE '17 — CONFIRMED (acronym correct); claim PARTLY

Chun Yang, Xianhua Liu, Xu Cheng. "Content Look-Aside Buffer for Redundancy-Free Virtual Disk I/O and Caching." VEE '17. DOI 10.1145/3050748.3050762.
ACM: https://dl.acm.org/doi/10.1145/3050748.3050762 (403 to the fetch tool; abstract via OpenAlex; full text not opened)

Attaches persistent fingerprints to virtual disk blocks to detect I/O redundancy before disk access; serves redundant reads from guest page cache via page sharing; KVM prototype. Abstract reports I/O and cache dedup (eliminates 94.9 to 98.5% of read requests and saves 50 to 100% cache memory on boot and app launch; 8 to 16x VM density) but no storage capture.

Correction: "does not measure its capture" is true for storage capture; it does measure I/O and cache redundancy. Say which.

## 12. Henson, HotOS '03 — CONFIRMED

Val Henson (Sun Microsystems). "An Analysis of Compare-by-hash." HotOS IX, May 2003.
https://www.usenix.org/legacy/events/hotos03/tech/full_papers/henson/henson.pdf

## 13. tvix-castore — CONFIRMED, with a rename and a numbers caveat

The crate is now snix-castore in https://git.snix.dev/snix/snix (castore left the tvl depot). Verified from a clone of branch canon: `snix/castore/Cargo.toml` depends on `fastcdc` (tokio feature) and `blake3`; `snix/castore/src/blobservice/object_store/mod.rs` uses `fastcdc::v2020::AsyncStreamCDC` with min/avg/max chunk sizes and `blake3::hash` per chunk. The snix docs page https://snix.dev/docs/components/castore/blobstore-chunking-verified-streaming/ names BLAKE3 as the hash.

Caveat for section 5.3's "no numbers have been published": Replit reports its 6 TB cache of Nix store paths shrinking to 1.2 TB with tvix-store. https://replit.com/blog/tvix-store. Not a paper, but a published number.

## 14. Xet — CONFIRMED for mechanism; ratio unverified

Hugging Face docs: content-defined chunking via rolling hash, about 64 KB chunks (8 to 128 KiB), grouped into 64 MB xorbs, content-addressed store keyed by hash, dedup across repositories. https://huggingface.co/docs/hub/xet/deduplication and https://huggingface.co/docs/xet/en/deduplication

That page gives no fleet-wide ratio. A figure of "62% deduplicated uploads, 1.5 TB saved on 912 GB uploaded" appeared in a search snippet attributed to Hugging Face, but I did not open its source. Treat as unverified. ZipLLM (item 15) states Hugging Face runs two-stage file-level plus chunk-level CDC dedup. Origin paper: Low et al., "Git is for Data," CIDR 2023, https://www.cidrdb.org/cidr2023/papers/p43-low.pdf (not opened).

## 15. ZipLLM — CONFIRMED; venue is NSDI '26, not 2025

Zirui Wang, Tingfeng Lan, Zhaoyuan Su, Juncheng Yang, Yue Cheng. "ZipLLM: Efficient LLM Storage via Model-Aware Synergistic Data Deduplication and Compression." USENIX NSDI '26 (May 2026). arXiv preprint 2025.
https://www.usenix.org/system/files/nsdi26-wang-zirui.pdf
https://github.com/ds2-lab/ZipLLM

Whole-file dedup across all public Hugging Face model repos (Table 2): 5,688,779 files; 1,182,818 exact duplicates; 11.89 PB total; 0.97 PB saved (8.2%); 506,337 repos (33.2%) contain at least one duplicate file. On their 3,048-model, 43.19 TB sample, file dedup leaves 41.80 TB (3.2%). Tensor-level dedup plus BitX compression reaches 54%. Supports "whole-file dedup collapses on model corpora".

## 16. despairlabs — CONFIRMED

"OpenZFS deduplication is good now and you shouldn't use it." 2024-10-27.
https://despairlabs.com/blog/posts/2024-10-27-openzfs-dedup-is-good-dont-use-it/

Describes the dedup log, `dedup_table_quota`, `zpool ddtprune`, and the live entry shrinking from 424 to 216 bytes. Verdict quotes: worth it only "if you have a very very specific workload where data is heavily duplicated and clients can't or won't give direct 'copy me!' signal" and "it is still only of benefit if you have a truly enormous amount of data, that gets copied a lot, and aren't able to take advantage of other 'zero-copy' options." Recommends block cloning instead. "Probably not, for most people" is a fair paraphrase.

## 17. EPIC, ICML '25 — CONFIRMED

Junhao Hu, Wenrui Huang, Weidong Wang, Haoyi Wang, Tiancheng Hu, Zhang Qin, Hao Feng, Xusheng Chen, Yizhou Shan, Tao Xie. "EPIC: Efficient Position-Independent Caching for Serving Large Language Models." ICML 2025, PMLR 267:24391–24402.
https://proceedings.mlr.press/v267/hu25j.html
https://arxiv.org/abs/2410.15332

## 18. casync, restic, borg — CONFIRMED

All chunk-level content-defined chunking over content-addressed chunks.

- casync: buzhash rolling-hash chunker; SHA512/256 chunk digests (SHA256 optional). https://github.com/systemd/casync
- restic: "content-defined-chunking (CDC) based on a rolling Rabin Hash." https://github.com/restic/chunker
- borg 1.x: default buzhash, plus fixed. https://borgbackup.readthedocs.io/en/stable/internals/data-structures.html
- borg 2 (master docs): default fastcdc; also buzhash, buzhash64, fixed, and AES-based variants. https://borgbackup.readthedocs.io/en/master/internals/data-structures.html

## 19. NVMe pricing — PARTLY; wording needs qualifiers

2023:
- Tom's Hardware, June 2023: SSD prices average $0.06/GB ($60/TB). https://www.tomshardware.com/news/ssd-prices-sink-june-2023
- TechRadar, Black Friday 2023: Teamgroup Gen3 NVMe at $39/TB. https://www.techradar.com/pro/black-friday-2023-is-when-pcie-ssd-killed-sata-ssd-teamgroups-gen3-ssd-grab-top-spot-at-dollar39-per-terabyte
- cheapestssd.com: "mid-2023 lows were $30–40/TB."

"Near $50/TB in 2023" is defensible for cheap consumer NVMe.

August/September 2026:
- cheapestssd.com, 2026-09-01: cheapest NVMe $105/TB (ADATA Legend 710 2TB, $209.99, Gen3); cheapest SATA $94.28/TB (Crucial BX500 4TB). https://cheapestssd.com/
- Tom's Hardware tracker, updated 2026-09-01, premium Gen5 drives only: 4 TB from $140/TB (Crucial T705, $559), 2 TB from $190/TB, 1 TB from $249/TB; lowest-ever prices $80–90/TB. https://www.tomshardware.com/pc-components/ssds/ssd-price-tracking-2026-lowest-price-on-every-m-2-ssd
- GamersNexus, 2026-04-02: average 2 TB NVMe $168.75 in Nov 2025 vs $357.50 in Mar 2026. https://gamersnexus.net/features/ssds-wtf

"Cheapest drives past $100/TB as of August 2026" holds for NVMe; the cheapest SATA drive is still under $100/TB.

NAND market and relief timing:
- TrendForce, 2026-02-02: 1Q26 NAND contract prices +55–60% QoQ. https://www.trendforce.com/presscenter/news/20260202-12911.html
- TrendForce, 2026-07-03: 3Q26 NAND contract prices +10–15% QoQ. https://www.trendforce.com/presscenter/news/20260703-13134.html
- Neither TrendForce release names a relief date.
- NAND Research, 2026-02-28: new fabs "will not reach meaningful production volume until late 2026 or 2027"; quotes Micron's CEO on tightness "continuing into 2027." https://nand-research.com/memory-flash-crisisc-update-march-2026/

Correction: attribute "no relief before 2027" to NAND Research and Micron's CEO, not TrendForce. Do not cite cheapestssd.com's quarterly $/TB history table; it looks auto-generated. diskprices.com did not render for me.

## 20. Anthropic prompt-cache read pricing — CONFIRMED with an exception

Cache reads are 0.1x base input price. 5-minute cache writes 1.25x; 1-hour cache writes 2x. Exception: Claude Fable 5.1 and Claude Mythos 5.1 cache reads are 0.025x.
https://platform.claude.com/docs/en/docs/build-with-claude/prompt-caching

## Could not open

USENIX and ACM HTML pages (403; PDFs fetched with curl instead). ACM full text for Jayaram (item 4) and CLB (item 11). code.tvl.fyi (Anubis block; used the snix clone instead). diskprices.com. The Hugging Face source for the 62% Xet figure.
