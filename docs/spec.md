# The half-life of a clone: measuring the duplicate data copy-on-write cannot reach in VM fleets

[[Alternates: "Duplicate data that copy-on-write cannot reach: a measurement on VM fleets." Or "How fast does a clone drift? Copy-on-write sharing, dedup, and time in VM fleets." Pick the one you can say out loud.]]

CS 4993, fall 2026. Research spec, v7.

Supersedes v6 and the playbook spec. The phase 3 daemon design and the distribution pages move to `docs/history/`.

Review that drove this version: `docs/review/`. Your v6 edits are backed up in the session scratchpad.

## Summary

Copy-on-write filesystems share data along a clone's history.

Two disks that started as copies of one image share blocks until each one writes over them.

Two disks that were installed separately and then updated to the same package set hold identical bytes too, but no snapshot or clone can share them, because neither copy descends from the other.

We will call the second kind cross-lineage redundancy.

Published dedup ratios on VM fleets do not separate the two, leading to measurement bias: an operator reading a ratio cannot tell how much of it clones would have given them for free.

This study isolates the two concepts.

For a fleet of clones under a normal update cadence, we measure how fast copy-on-write sharing decays with time since clone, how much of what it loses an aligned dedup table (ZFS dedup, dm-vdo) recovers, and how much is left that ONLY content-defined matching could ever reach.

The time axis is built from dated public images and package archives, so the curve is longitudinal and anyone can rebuild it.

Real fleets validate the curve and we measure what the aligned tier costs on stock backends under identical guests and workloads.

No new storage system is built for this study and the output is a curve an operator can read against their own fleet's age, and a rule for what to turn on.

## 1. The question

Take two VMs.

Each runs `apt upgrade` and downloads the same packages.

Their disks now contain the same bytes.

If they were cloned from a common image, the unchanged parts of that image are still shared, but the new package files are not, since each VM wrote them independently.

Copy-on-write shares data that was copied.

It by design cannot share data that became equal afterward.

Stock systems can reach some of that data.

OpenZFS dedup, dm-vdo, bees, and duperemove all attach a content hash to each block and share blocks with equal hashes, regardless of where they came from.

They work at a fixed, aligned block size (4K, or the zvol's volblocksize), so they catch duplicates that land on the same block boundary in both copies and miss the ones that do not.

Content-defined chunking (CDC) cuts boundaries from the data itself and catches the shifted ones, at the cost of a different storage design.

So a fleet's duplicate bytes fall into three groups, each needing a different mechanism:

1. Reachable by copy-on-write. Identical and in place relative to the base image. Free with clones and snapshots.
2. Cross-lineage, aligned. Identical at a fixed block boundary in both copies. Reachable by an aligned dedup table.
3. Cross-lineage, shifted. Identical content at a different offset. Reachable only by content-defined matching.

The ZFS community already knows the first group exists.

Its advice (despairlabs, 2024) is to use clones and block cloning for the copy case, and dedup only when clients cannot send a copy signal.

What nobody has is the number: how large each group is on a real fleet, and how the sizes move as the fleet ages.

Klara's fast-dedup article gives the anecdote: a server's dedup ratio falling from 5x to 1.15x "in a couple of months" as identical VMs diverged.

That is the curve this study measures.

## 2. Why the sizes matter

Raw capacity is cheap most years.

Consumer NVMe averaged about $60 per TB in mid-2023 and bottomed near $40 per TB that November; the 2025–26 NAND shortage put the cheapest NVMe at $105 per TB on 2026-09-01, and Micron's CEO expects tightness into 2027.

[[Sources in docs/review/citations.md item 19.]]

At either price, halving a small fleet's bytes saves little money at rest, and this study does not depend on the price cycle.

The result an operator acts on is different.

If clone sharing decays to a floor within months, template rebuild cadence matters more than any dedup setting.

If an aligned table at 4K recovers nearly everything COW loses but the same table at 16K does not, volblocksize is the decision, and it is one operators currently make by default.

If the shifted group is small on Linux guests, no content-defined system is worth building for that class, and the study says so with the number.

The cost side reports what the aligned tier costs in latency, memory, and write amplification, so the rule comes with its price.

## 3. Related work

[[Every citation was verified on 2026-09-01; see docs/review/citations.md and novelty.md for URLs and what was actually opened. Jayaram's body text and CLB's full text were not opened. Read them before this section is final.]]

### Nearest precedents

Zhang et al. (IEEE CLOUD '12; MSST '15) decomposed backup duplicates from about 2500 Alibaba VMs by mechanism: dirty bits against the parent snapshot took 10 TB per machine to 24%, similarity search against the parent to 12%, cross-VM dedup to 8.6%.

That is a lineage-versus-content split on a real fleet.

It differs from this study in four ways: lineage there is one VM's own snapshot chain rather than clone siblings; it measures backup streams rather than images at rest; chunking is variable, so there is no aligned tier; and there is no time axis.

Atkinson et al. (NSDI '14) measured in-place similarity of 267 Emulab images to an inferred base image, with most images above 50% and a peak at 60 to 80%.

Lin et al. (TridentCom '15) measured global dedup on an overlapping Emulab corpus at 3 to 5x on top of 3x compression.

Between them both halves of this study's subtraction exist on one image catalog; nobody subtracted, and neither has a time axis.

### Dedup ratio studies

Meyer and Bolosky (FAST '11) compared whole-file and block-level dedup on 857 desktops.

Jin and Miller (SYSTOR '09) measured VM disk images, including independently installed images with the same packages, and found fixed-size blocks nearly match variable-size chunking.

DeDe (ATC '09) ran out-of-band fixed-4K dedup on VMFS and reported 80% of a 113-VM VDI footprint as duplicate.

Jayaram et al. (Middleware '11) measured similarity within and across 525 production cloud images and report that image creation time affects similarity.

El-Shimi et al. (ATC '12) describe Windows Server's post-process CDC dedup with a 15-server corpus study.

Zhao et al. (IEEE CLUSTER '19) measured Docker Hub after factoring out layer sharing and found about 97% of files duplicate; that is the lineage-versus-content split for containers, at file level.

Sun et al. (MSST '16) tracked dedup ratios over 21 months of daily home-directory snapshots, which is a time axis without a mechanism split.

None of these separates what a clone would have shared from what arose independently on VM images, and the VM corpora are from 2009 to 2011.

### Dedup cost studies

iDedup (FAST '12), Dmdedup (OLS '14), and dm-vdo (mainline since Linux 6.9) measure what inline fixed-block dedup costs on primary storage.

None compares capture against a copy-on-write baseline.

No controlled p99 comparison of dm-vdo against ZFS fast dedup exists in print; Red Hat's VDO guide tells you to watch p99 on 4K random write and publishes no numbers.

### Systems

OpenZFS has had clones since its first release, dedup since 2009, block cloning since 2.2, and fast dedup since 2.3.

There is no published split of what clones and the dedup table each capture.

NetApp ONTAP reports snapshot, FlexClone, dedup, and compression efficiency as separate ratios per aggregate, so the split is an operational concept in one vendor's stack, with no public dataset.

Chunk-level content-addressed stores exist at scale: TiDedup (ATC '23) in Ceph, HYDRAstor (FAST '09), casync, restic, borg, Xet, snix-castore.

They report production ratios (Replit: a 6 TB Nix store to 1.2 TB; nixbuild.net: 6.55x chunked against 2.69x zstd) and none has a copy-on-write baseline to subtract.

ZipLLM (NSDI '26) measured dedup granularity across all public Hugging Face model repositories: whole-file dedup saves 8.2%, chunk-level far more.

The model class is therefore already measured and is not a corpus here.

### The gap

Filesystem developers ship clones and a dedup table and publish neither's share.

Backup tools ship chunking and never had a clone baseline.

The measurement in between, on VM images, against time, has not been done.

## 4. Hypotheses

H1. Under a normal monthly update cadence, the share of a Linux clone's non-zero bytes still shared with its base image falls below half within twelve months of clone, and the decay has a floor set by the packages the cadence never touches.

[[Twelve months and half are chosen, not derived. They are written down here before any data exists and do not move. If you want a defensible origin, tie them to the LTS point-release cadence.]]

H2. On Linux guests, an aligned dedup table at 4K recovers at least 90% of the bytes copy-on-write lost, so the shifted residue is under 10% of cross-lineage bytes.

At 16K, the OpenZFS zvol default, the same table recovers materially less, and the 4K-to-16K gap is the result.

[[The 4K half of H2 is close to foregone on ext4 and XFS guests: 4K blocks, 1 MiB partition alignment, whole-file package writes. Say so in the paper. The 16K half is not known and is what operators actually run.]]

H3. On encrypted guests (LUKS, BitLocker), cross-lineage sharing is zero at every granularity.

Stated so the paper can say which fleet classes the rule does not apply to; costs nothing to measure.

A flat curve, a small 4K-to-16K gap, or a large shifted residue each reverses a recommendation and still stands as a result.

## 5. Method: the census

The census is offline analysis of disk images at rest.

It needs no running guest and no root on the analysis node; guest filesystems are read with libguestfs or e2fsprogs in read-only mode for allocation maps and file boundaries.

It produces the split from section 1 for each corpus at several points along the corpus's timeline.

### 5.1 Decomposition

Every byte range in every image is classified into exactly one of five categories.

- Zero or unallocated. Excluded from every ratio and reported separately. Unallocated comes from the guest filesystem's allocation map, not from a zero test, so deleted-file remnants do not inflate the other categories.
- Unique. Appears once in the corpus.
- Reachable by copy-on-write. Identical and in place relative to the image's dated base. The ceiling for any clone or snapshot system.
- Cross-lineage, aligned. Not reachable by COW; identical at an aligned block boundary in another image. Reported at 4K and at 16K.
- Cross-lineage, shifted. Not reachable by COW and not aligned; the same 4K of content appears at some other byte offset in another image.

The shifted category is defined by a rolling-window oracle (rsync-style weak checksum at every offset, strong hash on candidate hits), not by running a chunker.

That is the ceiling of any content-defined scheme.

FastCDC at 8K and 16K mean is run as well and reported as a practical figure beneath the ceiling.

[[The v6 definition, "found by CDC but not by 4K aligned," with CDC at 16K mean, was coarser than the aligned arm and could go negative per region. This fixes it.]]

Gate G1 is that the five categories sum to 100% of bytes for every corpus at every time point.

Duplicates within a single image are included in the categories above and also reported as their own column, as Meyer and Bolosky and Jayaram et al. did.

Compression is zstd, measured in both orders relative to dedup (A7).

A sample of every hash match is verified byte for byte and the sample size is reported (A3).

### 5.2 The time axis

The share reachable by copy-on-write is a function of time since clone.

A freshly cloned fleet is entirely lineage.

A fleet a year into independent update cycles is mostly cross-lineage.

The operator's question is when their fleet crosses over, so every category is computed at each point along the timeline and the output per corpus is a curve.

The axis is longitudinal, not a cross-section: the same image is followed through its own history.

[[v6 aggregated real-fleet images by age at one instant. Old VMs are different VMs from young ones. A reviewer kills that curve.]]

A second axis is template rebuild cadence.

Sibling clones share only what the base held at clone time, so a base that is rebuilt every k months and re-cloned changes the curve.

The census runs it at never, 6 months, and 3 months.

Snapshot cadence on the clones themselves does not enter; snapshots taken after the clone never create sharing between siblings.

### 5.3 Corpora

Longitudinal Linux fleets, built from dated archives.

Ubuntu publishes dated cloud images and keeps them for years; snapshot.debian.org serves the archive as of any date.

An image installed as of T0 and upgraded monthly against the archive as of T1, T2, and so on replays a real update history exactly, with the base image at T0 kept as the declared ancestor.

N such clones with scripted per-VM drift (distinct hostnames, logs, a few installed packages) form a fleet.

This corpus is the primary source of the H1 and H2 curves, because it is longitudinal, dated, and rebuildable by anyone with one command (gate G2).

Convergent installs, as the control.

N independent installs of the same release updated to the same package set, with no common ancestor.

This is Jin and Miller's setup and lineage's structural blind spot; the aligned and shifted categories here bound what any fleet can show.

Real fleets, as validation.

The curve from dated archives predicts what a real fleet at a given age should show; real fleets test the prediction at single points.

Candidates, in order of how likely they are to say yes: the author's own machines; one or two Proxmox homelab donors; a research lab's Proxmox host where a student holds root; a university OpenStack or Proxmox cloud on Ceph RBD, where every VM is an `rbd clone` and `rbd info` records the parent and creation time.

Full clones on Proxmox, libvirt, and VMware record no ancestry, so a real fleet's base is reconstructed as the dated cloud image of its release, the same way the longitudinal corpus does it.

Encrypted guests, one pair, to state H3 with a number.

Windows guests are a stretch corpus: one Windows Server evaluation image pair updated across months, since WinSxS hardlinks and delta compression make the result far less predictable than apt.

Model corpora are dropped; ZipLLM measured them.

Nix store generations are dropped from the plan and listed in the cut order as the first thing to add back if hours remain; Nix has no copy-on-write lineage, so its split is whole-file versus chunk, a different axis.

### 5.4 Phase 0: the cheap answer first

Before the pipeline exists, `zdb -S` on a ZFS pool holding the cloned fleet gives the aligned cross-lineage figure at recordsize in one command.

Pool traversal starts each dataset at its previous-snapshot txg (`traverse_pool` in `dmu_traverse.c`), so blocks a clone inherited from its origin are counted once, and the simulated dedup ratio is duplicates beyond what clones already share.

[[Verified in source on 2026-09-01. Confirm with the five-minute test: clone twice, write the same file into both, `zdb -S`.]]

`duperemove --dry-run` gives the same on XFS.

Phase 0 runs in week 1 on the synthetic fleets, gives the first number, and is the tool a ZFS donor runs themselves.

### 5.5 Donor protocol

The census runs at the donor's site, as root, against a consistent snapshot of each image.

What crosses the boundary is the decomposition table: byte totals per category per time point.

No image bytes and no chunk hashes leave, because the census never needs cross-donor matching; every match is within one fleet.

[[Chunk hashes of a disk fingerprint every installed package version. v6 shipped hashes and called the privacy conversation short. It is not.]]

The pipeline and the protocol are published so a donor can audit what runs.

For ZFS donors the ask is the phase 0 command, which needs no new binary on their host.

If no external donor lands, the study stands on the longitudinal corpus, the control, and the author's machines, and says so.

### 5.6 Pipeline

A few thousand lines over the `blake3` and `fastcdc` crates.

Output is one aligned-hash stream per image (about 8 GB per TB at 4K with 32-byte hashes), matched by external sort-merge rather than an in-memory table, so a 10 TB fleet fits on one node.

The shifted oracle is the expensive step: one rolling checksum per byte offset checked against the aligned-hash set, roughly three core-hours per TB, parallel across the node.

[[Unmeasured; from arithmetic at 10^8 lookups per second per core. Measure in week 2 and replace.]]

Analysis is `uv run` Python over per-corpus tables.

First numbers from phase 0 in week 1; first pipeline curves in week 3.

### 5.7 Side results

The census also settles, on its corpora: whether compression captures most of dedup's win; whether fixed blocks still approximate CDC on Linux VM images (Jin and Miller 2009, retested at 4K and 16K); what fraction of observed sharing an explicit copy signal could ever have declared; and how the intra-image column compares to Jayaram et al.

## 6. Cost of aligned dedup on stock backends

Phase 2 writes no new code.

Same stock QEMU, same guest, same NVMe device, three backends an operator can turn on today.

### 6.1 Why stock systems

The aligned tier is exactly what shipping systems reach.

Measuring it on those systems answers the operator's question directly.

A research daemon would measure a design nobody deploys.

### 6.2 Backends

Same QEMU configuration and cache mode (A4) for all three; only the storage behind the virtio-blk device changes.

R0. Raw file on XFS on the dedicated NVMe, through QEMU's raw driver. The control. No dedup anywhere in the path.

R1. Zvol on a ZFS pool on the same NVMe device, created and destroyed per run, opened directly by QEMU as a block device. Stock OpenZFS 2.3 or later with fast dedup, which replaces the legacy DDT's random writes with a sorted log flush and adds a quota and pruning. Configuration: `feature@fast_dedup` enabled; `dedup=blake3` (not `dedup=on`, which silently uses SHA-256 regardless of the checksum property); `volblocksize=16K` as the primary arm and `4K` as the second; `dedup_table_quota` unset and `zpool ddtprune` never run during a measurement, both recorded; `compression=zle` outside the labeled compression arm, so zero blocks do not collapse onto one DDT entry with a refcount in the millions; DDT memory from `zpool status -D`. OpenZFS direct IO does not apply to zvols or with dedup, so R1 is ARC-backed in every arm, and the paper says so.

R2. Raw file on XFS on top of dm-vdo on the same NVMe. Inline fixed-4K dedup and compression in the kernel, mainline since 6.9. Dedup on, compression off outside the compression arm, index memory from `vdostats`. The cleanest comparison in the set: the same file and filesystem type as R0 with one device-mapper layer added. R2's XFS is its own filesystem instance on the vdo device, not R0's.

R3, optional. Raw file on R0's XFS with post-process duperemove at `-b 4k --dedupe-options=partial`, since the default is 128K extent matching. After the pass, guest writes into shared extents pay XFS copy-on-write, and that post-pass write latency is the honest price of the "free" option. R3 is in the cut order, not the plan.

R0 against R2 is the cost of inline aligned dedup with everything else held constant.

R1, read against the census at the matching volblocksize, shows how much of the aligned tier a deployed dedup table reaches against what the census says it could, and the 16K arm is the number operators run today.

R1 differs from R0 and R2 in kernel boundary, caching, and allocation, so it is a case study beside the controlled pair, and the paper attributes deltas accordingly.

### 6.3 Workloads

fio: 4K random write and read at QD1 and QD32, 128K sequential.

N-clone boot storm at N = 4, 16, 32.

Replay of one longitudinal fleet from the census onto N guests, at two points on its curve.

No kernel build, no synthetic stress workload.

### 6.4 Metrics

Latency. Guest p50 and p99 write and read against R0, at least five repetitions, variance beside every number. Reported first. A backend that captures everything and doubles p99 is a different result from one that captures everything at parity, and the table shows which before it shows how much was captured.

Storage. Consumed after ingest, against the census's prediction for the same images at the backend's block size.

Index memory. DDT or vdo index bytes per stored TB.

Write amplification. Device bytes written per guest byte written, from NVMe counters. For R1 this includes the dedup log flush; for R2 the index and block map.

Cache, as one paragraph rather than a column. The Linux page cache is keyed per inode, so on R0 and R2 identical blocks in two files occupy two sets of pages regardless of what the block layer dedups, and dm-vdo has no read cache. ZFS's ARC is keyed by physical block, so deduped blocks share one entry. R1 is the only backend in the set whose host cache shares anything, and the paper states that as an operator fact and measures host device reads per guest byte in the boot storm to show it.

Transfer is not a phase 2 metric. OpenZFS removed deduplicated send in 2.0, dm-vdo has no replication, and reflinks do not survive rsync. Moving unique bytes only is a property of content-addressed stores (casync, restic, Xet) and belongs in future work.

### 6.5 Instrumentation and controls

All backends are observed at the guest boundary (fio's own latency histograms, guest-side blktrace for the boot storm) plus host device counters.

`zpool` statistics and `vdostats` are supplementary.

Controls: pinned vCPUs, performance governor, discarded warm-up, fresh filesystem or pool per repetition, page cache and ARC bounded to the same size by cgroup and `zfs_arc_max`, at least five repetitions.

Gate G3 is a complete table: three backends, identical workloads, no empty cells.

### 6.6 What phase 2 cannot say

Nothing here reaches the shifted category.

If the census says it is small on Linux guests, which H2 predicts, this table is the whole cost side and the study is complete.

If it is large, the paper says a content-defined block backend is worth building and stops there.

## 7. Plan

### 7.1 Hardware

x86-64 bare metal.

Phase 1 needs one node and no root; phase 2 needs one node with a dedicated NVMe device.

Primary testbed: CloudLab c6525-100g (Utah), one node at a time.

Per node: one AMD EPYC 7402P (24 cores, 2.80 GHz), 128 GB ECC DDR4-3200, two 1.6 TB PCIe 4.0 NVMe SSDs.

One NVMe device holds the system and results; the second is dedicated to the store under test.

CloudLab is free for research; a project is started by a faculty member and reviewed by CloudLab staff, and the faculty lead then approves members.

[[The faculty sponsor has to open the project. Ask before Sep 9.]]

Reservations expire at 16 hours by default with extensions on request, so every phase 2 run is scripted to complete inside one reservation.

dm-vdo needs a 6.9 or later kernel and OpenZFS 2.3 is a source build; both are done once and imaged in week 7.

Fallback: one OVHcloud Advance-4 2026 bare-metal server, AMD EPYC 4585PX, 16 cores, 64 GB DDR5 ECC base, 2 × 960 GB NVMe.

No phase uses the network for data.

Every figure in the paper is measured on the testbed.

### 7.2 Schedule

Fourteen weeks at roughly eight hours each is about 110 hours.

The plan below fits that number because the pipeline is scoped to three categories plus the oracle, phase 2 is three backends, and there is no system to build.

| Weeks | Phase | Result |
|---|---|---|
| 1–2 | 0 | thresholds written (G4); synthetic fleets built; `zdb -S` and `duperemove --dry-run` numbers; donor asks sent |
| 3–5 | 1 | pipeline; longitudinal corpus from dated archives; first curves; shifted oracle measured |
| 6–7 | 1 | convergent control; encrypted pair; own machines and any donor; H1, H2, H3 verdicts |
| 8–10 | 2 | R0, R1 at 16K, R2; fio, boot storm, fleet replay; cost table (G3) |
| 11–12 | 2 or 1 | R1 at 4K, then R3, then Nix, in that order, as hours allow; otherwise another real fleet |
| 13–14 | | report; reproducibility pack (G5) |

### 7.3 Gates

G1. The decomposition is exhaustive and disjoint: categories sum to 100% of bytes per corpus at every time point.

G2. One command rebuilds the longitudinal corpus from dated public archives on any machine.

G3. The cost table is complete: three backends, identical workloads, latency, storage, index, and amplification columns, no empty cells, variance beside every number.

G4. H1, H2, and H3 thresholds are written at the end of week 2 and do not move.

G5. One command reruns the census on any directory of images with an ancestry file; one command reruns the cost table on a second node.

### 7.4 Cut order

If the schedule slips, items come off from the top.

Nix corpus.

R3.

R1 at 4K.

The Windows pair.

Never the longitudinal corpus, never the control, never R0 against R2.

### 7.5 Risks

No external donor.

The longitudinal corpus does not depend on one; donors are validation points.

The ask goes out in week 1 to several candidates at once, and the author's own machines are the floor.

Corpus realism.

Scripted drift is not real drift.

The mitigation is the dated-archive replay, which reproduces real update history, plus every real fleet that lands, plus publishing the scripts so the classes can be criticized (A8).

Pipeline overrun.

The oracle is the only expensive step and is measured in week 2; if it is too slow, it runs on a sample of images and says so.

Novelty.

Swept on 2026-09-01 (docs/review/novelty.md).

Nearest precedents are Zhang et al. and the Emulab pair, both cited in section 3.

OpenZFS developer summit talks were checked by title only; mailing lists and the issue tracker were not swept and must be before related work is final.

### 7.6 Logistics

CS 4993, 1 credit.

Planned effort is roughly 8 hours a week; the credit understates the work.

Expectations in writing before Sep 9.

Thirty minutes of sponsor time every two weeks, with the week-2 threshold sign-off as a scheduled meeting.

## 8. Scope and assumptions

A1. Workload class is hosts serving multiple guests from local flash, homelab to rack scale. Array economics are out of scope.

A2. Experiments run at single-digit TB. Index and amplification costs are reported as formulas with measured constants; any 100 TB figure is labeled an extrapolation.

A3. Equal BLAKE3 (256-bit) implies equal bytes. The census verifies a sample of matches byte for byte and reports the sample (Henson, HotOS '03).

A4. The guest contract is virtio-blk with a volatile write cache: an acknowledged FLUSH is durable and nothing else is. Every backend runs under the same QEMU cache mode.

A5. One image, one writer. Shared-disk clustering is out of scope.

A6. Dedup side channels and convergent-encryption probing are documented and excluded. The donor protocol (5.5) moves tables, not hashes, so this assumption does not extend to donors.

A7. Compression is zstd, measured in both orders relative to dedup. All-zero and unallocated ranges are excluded from every ratio and reported separately.

A8. Corpora represent their declared classes only. Build scripts and the donor protocol are published; results are per class; no universal ratio is claimed.

## 9. What comes out

A measurement paper with a curve an operator can read against their own fleet's age and a rule for what to turn on: rebuild templates every N months, pick 4K or 16K, skip dedup on encrypted fleets.

A cost table for ZFS fast dedup and dm-vdo on identical hardware and workloads, which as far as we can find has not been published side by side.

A published pipeline and a dated-archive corpus builder so the measurement can be rerun on any fleet or any release.

An answer, from data, to whether content-defined matching on Linux guest images reaches enough beyond an aligned table to justify building for it.

## 10. Future work

Content-defined block storage.

If the shifted residue is large on some class, the two-tier design in `docs/history/` (staging log ahead of a content-addressing compactor, over vhost-user-blk) is the instrument to price it, and transfer becomes measurable there because a content-addressed store moves unique bytes only.

Distribution of that store is a known property (HYDRAstor, Ceph's chunk pool) and follows from it.

KV caches.

The same split appears in LLM serving.

Prefix caching in vLLM, SGLang, Mooncake, and LMCache names cached KV blocks by a hash chain over the whole token history, so two requests share KV only along a common prefix; that is lineage.

The same document after two different preambles is computed twice; that is the cross-lineage case.

Position-independent caching (EPIC, ICML '25, and successors) is the mechanism that would reach it, at a recompute cost nobody has bounded, and no trace study has decomposed prefix reuse from non-prefix reuse.

That census is the next study.

## Editor's notes

- Title.
- H1 numbers (twelve months, half). H2's 90%. Written down by week 2 and then frozen.
- Read Jayaram et al. and CLB in full; both were verified from abstracts only.
- Run the `zdb -S` five-minute test before citing the traversal behavior.
- Measure the oracle's cost in week 2 and replace the arithmetic estimate.
- Ask the sponsor to open the CloudLab project.
- Move `playbook/SPEC.md`, `SPEC-v1.md`, and page 03's daemon design to `docs/history/`; fix the README.
