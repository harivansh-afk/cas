# Novelty sweep: lineage versus content dedup split on VM fleets

Sweep date: 2026-09-01. Question checked: has anyone measured, on VM disk image fleets or any corpus, how duplicate bytes split between (a) duplicates that copy-on-write clones already share, (b) independently arising duplicates at an aligned fixed block boundary, and (c) independently arising duplicates only content-defined chunking catches, and has anyone reported that split as a function of time since clone.

Verdict: the gap is real. No study found splits duplicate bytes three ways, and none reports any such split against time since clone. The nearest work measures one side of the split without the other.

"Opened" means the fetched PDF or page text was read. "Not opened" means abstract or search snippet only.

## Near misses, in order of closeness

### 1. Zhang, Tang, Jiang, Yang, Li, Zeng (Alibaba backup decomposition)

- "Multi-level Selective Deduplication for VM Snapshots in Cloud Storage", IEEE CLOUD 2012. https://ieeexplore.ieee.org/document/6253550 (not opened; findings taken from the thesis below)
- "VM-Centric Snapshot Deduplication for Cloud Data Backup", MSST 2015. https://msstconference.org/MSST-history/2015/Papers/21.Zhang.pdf (opened)
- Wei Zhang, UCSB thesis, "Collocated Data Deduplication for Virtual Machine Backup in the Cloud". https://escholarship.org/content/qt03m00784/qt03m00784_noSplash_26d4a120ed99736c8fe7d7cb24c9e3e5.pdf (opened)

What it measured: Alibaba/Aliyun VM backup data, about 2500 VMs on 100 physical machines. Duplicates are removed in stages and the paper reports what each stage removes. Level 1 is segment dirty bits against the parent snapshot (same VM, previous version). Level 2 is similarity search against the parent snapshot's recipes. Level 3 is cross-VM dedup restricted to a popular data set (PDS).

Numbers, MSST 2015 Table III: dirty bits alone reduce 10 TB per machine to 24.14% of original; parent-snapshot similarity search takes it to 12.05%; cross-VM popular-chunk dedup at sigma 2% takes it to 8.6%. Thesis (chapter 4): "Level 1 segment dirty bits identify 78% of duplicate blocks. For the remaining dirty segments, block-wise full deduplication removes about additional 74.5% of duplicates. The final content copied to the backup storage is reduced by 94.4% in total." Thesis (chapter 5): "duplication ... can be categorized into inner-VM and cross-VM. Inner-VM duplication exists between VM's snapshots ... Cross-VM duplication is mainly due to widely-used software and libraries such as Linux and MySQL."

Verdict: partial overlap, and the nearest thing to a scoop. It is a lineage-versus-cross-VM decomposition on a real production fleet. Differences from the proposed study: lineage there is one VM's own snapshot chain, not clone siblings descending from a golden image; it is a backup stream, not images at rest; chunking is variable-size with a 4 KB mean, so there is no aligned versus unaligned tier; cross-VM search is capped at a popular-chunk set rather than exhaustive; there is no time-since-clone axis. Cite prominently and state the differences.

### 2. Atkinson, Wong, Ricci (Emulab in-place similarity to base image)

"Operational Experiences with Disk Imaging in a Multi-Tenant Datacenter", NSDI 2014. https://www.flux.utah.edu/paper/atkinson-nsdi14 (opened via https://www.flux.utah.edu/download?uid=198). USENIX copy: https://www.usenix.org/system/files/conference/nsdi14/nsdi14-paper-atkinson.pdf (not opened, 403).

What it measured: four years of Emulab image loads; 267 images available for block analysis. Defines the difference of two images as the set of allocated blocks of B whose content differs from A at the same index. Each user image's base is the facility image minimizing that difference (inferred, not declared). Section 3.4: "most are more than 50% similar, with a significant peak in the 60%-80% range" and "a significant tail of more than twenty images with very low similarity (below 10%) to their base images." Suggests deduplicating storage and differential loading as consequences.

Verdict: partial overlap. This is category (a), COW-reachable bytes (identical and in place relative to an ancestor), measured directly on a real image catalog. No content-dedup residual is measured, base is inferred, and the authors note the image files are "a snapshot of image contents at a particular point in time," so no time axis.

### 3. Lin, Hibler, Eide, Ricci (Emulab global dedup)

"Using Deduplicating Storage for Efficient Disk Image Deployment", TridentCom 2015. https://www.flux.utah.edu/paper/lin-tridentcom15 (opened via https://www.flux.utah.edu/download?uid=211)

What it measured: 430 Emulab Linux images (76 facility, 354 user-created, made 2002 to 2011, 217 GB allocated). Venti-backed store, fixed and variable chunking, 4 KB to 56 KB blocks. Compression gives 3x; dedup on top gives 3 to 5x; dedup ratio rises as block size falls. States "disk images are typically derived from other images by making small changes, there is significant duplication between an image and its 'children'" and cites Atkinson for the 60 to 80% figure, but reports only total dedup.

Verdict: partial overlap. Together with Atkinson this is the closest pair: lineage in-place sharing and global content dedup measured separately on overlapping Emulab corpora, never subtracted from each other. Also confirms fixed 4K approaches variable chunking on that corpus.

### 4. Jin and Miller (convergent installs without lineage baseline)

"The Effectiveness of Deduplication on Virtual Machine Disk Images", SYSTOR 2009. https://www.ssrc.us/media/pubs/082a25b906aa716ca3c2439b8c1889449ecac44c.pdf (opened; https://www.ssrc.ucsc.edu/papers/jin-systor09.pdf refused connections on the sweep date)

What it measured: prebuilt appliance images from public sites plus their own installs of Ubuntu and Fedora variants (locale, package sets, install order, different VMMs). Fixed-size chunks "achieving nearly the same compression rate as variable-length chunks." On independently installed images with the same packages: "installation order is relatively unimportant ... it is not necessary to base disk images on a 'gold' image; it suffices to use deduplication to detect identical disk blocks in the images."

Verdict: partial overlap. This is the convergent-installs control from spec section 5.3, measured without a lineage baseline and without a time axis. The H2 retest framing is correct.

### 5. Jayaram, Peng, Zhang, Kim, Chen, Lei (production image similarity)

"An Empirical Analysis of Similarity in Virtual Machine Images", Middleware 2011 industry track. https://research.ibm.com/publications/an-empirical-analysis-of-similarity-in-virtual-machine-images (abstract opened); https://dl.acm.org/doi/10.1145/2090181.2090187 (paywalled, body not opened)

What it measured: 525 production IBM cloud images, similarity within and between images. Abstract: similarity between pairs "exhibit high variance," an image is more similar to a small subset than to all others, and "the image creation time" is among the factors affecting similarity.

Verdict: partial overlap. A temporal factor is present, but as similarity versus creation date, not a lineage/content split versus age since clone. The body must be read before related work is final.

### 6. Zhao et al. and DupHunter (container analog)

- "Large-Scale Analysis of the Docker Hub Dataset", IEEE CLUSTER 2019. https://par.nsf.gov/servlets/purl/10167826 (opened). DOI https://doi.org/10.1109/CLUSTER.2019.8891000
- DupHunter, USENIX ATC 2020. https://people.cs.vt.edu/~butta/docs/atc2020-duphunter.pdf (opened); https://www.usenix.org/system/files/atc20-zhao.pdf (403)

What it measured: 1,792,609 layers, 5.28 billion files, 47 TB compressed. "Note that we only download unique layers," so layer sharing (the container analog of lineage) is factored out before the file analysis. Then: "only around 3% of the files are unique," overall file-level dedup ratio 85.69%, "90% of images contain more than 99.4% of files that are duplicated across images." DupHunter section 3.1: layer sharing "is not sufficient to effectively eliminate duplicates" because "layers often share many but not all files."

Verdict: partial overlap, different domain. This is the lineage/content split for containers: content dedup measured after the lineage mechanism is applied. File level only, no alignment question, no time axis. Supports the claim that the split exists for containers and not for VM images.

### 7. Sun, Kuenning, Mandal, Shilane, Tarasov, Xiao, Zadok (long-term backup dedup)

"A Long-Term User-Centric Analysis of Deduplication Patterns", MSST 2016. https://www.fsl.cs.stonybrook.edu/docs/msst16dedup-study/data-set-analysis.pdf (opened). Journal version: "Cluster and Single-Node Analysis of Long-Term Deduplication Patterns", ACM TOS 2018, https://dl.acm.org/doi/10.1145/3183890 (not opened)

What it measured: 21 months of daily home-directory snapshots, 33 users, 4,181 snapshots, CDC at 2 to 128 KB plus whole-file. Dedup ratio curves as snapshots accumulate (Figure 8), full versus incremental versus weekly-full backup ratios (Table II), per-user variance.

Verdict: different question. Has a time axis and a per-user versus shared distinction, but it is backup snapshot chains of home directories, not VM images, and there is no mechanism split.

### 8. Astronomy reflink versus dedup comparison

"Minimal Re-computation for Exploratory Data Analysis in Astronomy", arXiv 1809.01945. https://arxiv.org/pdf/1809.01945 (opened)

What it measured: one 41 GB repository of CASA intermediate products. ZFS block dedup: 6.5 GB allocated, 6.2x. btrfs cp --reflink=auto: 8.4 GB, 4.9x. "The copy-on-write mechanism is less efficient than de-duplication, but still very good."

Verdict: different question. The only place found with a COW baseline and a dedup figure on the same corpus; the difference is the cross-lineage tier by construction, but on radio-astronomy intermediates, one paragraph, no decomposition.

## Product accounting that reports the split without studying it

NetApp ONTAP `storage aggregate show-efficiency -details` reports Volume Deduplication Efficiency, Compression Efficiency, Snapshot Volume Storage Efficiency, and FlexClone Volume Storage Efficiency as separate ratios. Source: AWS FSx for ONTAP docs, https://docs.aws.amazon.com/fsx/latest/ONTAPGuide/view-storage-efficiency.html (opened). NetApp's own page https://docs.netapp.com/us-en/ontap-cli/storage-aggregate-show-efficiency.html returned 403. No published dataset of those fields was found, and the dedup figure is not split by alignment. Worth one sentence in related work: the industry measures the split per array and publishes nothing.

## ZFS side

- despairlabs, "OpenZFS deduplication is good now and you shouldn't use it", 2024-10-27. https://despairlabs.com/blog/posts/2024-10-27-openzfs-dedup-is-good-dont-use-it/ (opened). Still says you probably don't want dedup unless the workload is heavily duplicated and clients cannot send a copy signal. Points at block cloning as "a good chunk of the gain without the outsized amount of pain." Only the author's own pool: BRT used 292M, saved 309M, 2.05x, versus a dedup simulation showing little benefit. No fleet measurement, no split. Spec section 1's characterization holds.
- Klara Systems, "Introducing OpenZFS Fast Dedup". https://klarasystems.com/articles/introducing-openzfs-fast-dedup/ (opened). No dedup-versus-clone numbers. One anecdote: "we've seen a server go from 5x dedup ratio to 1.15x in just a couple of months" as identical VMs diverged. Unmeasured, but it is a time-since-clone observation and argues for H1's curve.
- Proxmox forum, "ZFS dedup on a Proxmox host in 2026, has fast dedup changed the recommendation?". https://forum.proxmox.com/threads/zfs-dedup-on-a-proxmox-host-in-2026-%E2%80%94-has-fast-dedup-changed-the-recommendation.185320/ (opened). A practitioner asks whether linked clones already get most of the benefit and whether the dedup factor decays with guest updates; no one answers. One poster's `zdb -S` on 8 TB mixed data: dedup 1.51x, compress 1.07x, and whole-file matching gets about 95% of that.
- Fast Dedup discussion, https://github.com/openzfs/zfs/discussions/15896 (not opened). OpenZFS Developer Summit talk lists 2022 to 2025 (https://openzfs.org/wiki/OpenZFS_Developer_Summit_2023_Talks, https://openzfs.org/wiki/OpenZFS_Developer_Summit_2024, https://openzfs.org/wiki/OpenZFS_Developer_Summit_2025, seen via search only): Fast Dedup and Block Cloning talks exist; none is a measurement of dedup savings beyond clones. Mailing lists and the issue tracker were not swept; spec section 8.5 still needs that.

## Model and Nix corpora

- ZipLLM, NSDI 2026. https://arxiv.org/html/2505.06252v2 (opened); https://www.usenix.org/system/files/nsdi26-wang-zirui.pdf. 3,048 Hugging Face repos, 43.19 TB after file dedup. File-level dedup 3.2%, tensor-level 8.3%, FastCDC chunk-level 14.8% with 12.5 TB of chunk metadata projected at hub scale. Aggregate by granularity only, no lineage attribution.
- Hugging Face Xet, "From Files to Chunks". https://huggingface.co/blog/from-files-to-chunks (opened). Fine-tunes and checkpoints show chunk dedup "in the range of 30-85%"; CORD-19 benchmark 8.9 GB to 3.52 GB versus Git LFS. No separation of exact copies from partial overlap.
- Hugging Face Xet, "From Chunks to Blocks". https://huggingface.co/blog/from-chunks-to-blocks (opened). 29 GGUF quantizations of gemma-2-9b-it: 191 GB stored as about 97 GB. No cause decomposition.
- NixOS Discourse, nix-casync thread, rickynils post 2021-12-21. https://discourse.nixos.org/t/nix-casync-a-more-efficient-way-to-store-and-substitute-nix-store-paths/16539 (opened). 671,652 nixbuild.net NARs, 8.0 TB: ZFS lz4 2.10x, ZFS zstd 2.69x, nix-casync uncompressed 3.22x, nix-casync zstd 6.55x.
- Replit, "Using Tvix Store to Reduce Nix Storage Costs". https://replit.com/blog/tvix-store (opened). 6 TB of store paths to 1.2 TB, 90% cost reduction. No ratio decomposition.
- The spec's claim that "no numbers have been published" for tvix-castore is too strong. "No decomposition has been published" is accurate.

## Not opened

- Meyer and Bolosky, "A Study of Practical Deduplication", FAST 2011 (USENIX 403, MSR page 404).
- DeDe, ATC 2009; El-Shimi et al., ATC 2012: not fetched. Nothing in their abstracts suggests a lineage split.
- Jayaram et al. body text (paywalled).
- Chinese-language search returned surveys and patents only.

## Overall verdict

The gap is real for VM images at rest. The closest prior work is Zhang et al. (CLOUD 2012, MSST 2015), which decomposes Alibaba VM backup duplicates into parent-snapshot lineage versus cross-VM content on a real fleet. Cite it as the nearest precedent with the differences stated: snapshot-chain lineage rather than clone-sibling lineage, backup stream rather than images at rest, variable chunking so no aligned/unaligned tier, capped cross-VM search, no time axis. The Emulab pair (Atkinson NSDI 2014 for in-place lineage sharing, Lin TridentCom 2015 for global dedup) is second-closest and shows both halves measured separately on one image catalog without subtraction. Zhao et al. and DupHunter did the analogous split for containers at file level. Nobody has the aligned/unaligned split or the time-since-clone curve on any corpus.

Spec edits implied:

- Add Zhang et al. and Atkinson et al. to section 3 as the nearest precedents.
- Soften "no numbers have been published" for tvix-castore to "no decomposition has been published."
- Add NetApp per-feature efficiency reporting as evidence the split is an operational concept without a public dataset.
- Keep the despairlabs characterization as is.
- Sweep OpenZFS mailing lists and the issue tracker before related work is final, per section 8.5.
