# Review of docs/spec.md (v6), skeptical PC-member stance

Verdict first. The study as designed has one publishable result, the decay of clone sharing over time (H1), wrapped around two that are not. H2 on Linux guests at 4K is Jin & Miller 2009 with a new label, and most of the phase 2 table is configuration work whose ordering is already known. Three definitional problems would sink it in review before anyone looked at numbers: category 3 is defined so that H2 cannot fail, "transfer" is a benefit none of the four stock backends can deliver, and the real-fleet time axis is a cross-section, not a curve.

## 1. Is H2 predictable?

Yes, on Linux guests with default filesystems it is foregone. ext4 and XFS use 4K blocks, partitions have been 1 MiB aligned since about 2010, dpkg and rpm write whole files, and the kernel zero-fills the tail of the last page. Two independent installs of the same package produce bit-identical 4K blocks at 4K-aligned offsets in both images. The only unaligned duplicates are sub-4K files (a rounding error by bytes) and in-place appends to logs and databases. Jin & Miller said it: "fixed-length chunks work well, achieving nearly the same compression rate as variable-length chunks" (https://ssrc.us/pub/jin-systor09.html).

It is worse than predictable. The spec's CDC arm uses a 16K mean with an 8K to 64K range, coarser than the 4K aligned arm. A 4K change inside a 16K CDC chunk loses 16K of dedup where fixed 4K loses 4K. So category 3, "found only by CDC," is measured with a tool that finds less than the aligned arm across most of the image, and the category can go negative per region. H2 at 90% is not a hypothesis under this definition. It is an accounting identity, and phase 3 is cancelled by construction. Fix: define the unaligned ceiling with a shifted-match oracle (rsync-style rolling 4K window, so a 4K aligned block in image A that appears at any byte offset in image B counts), or run CDC with a 4K mean so it is at least as fine as the aligned arm.

Is a well-measured confirmation publishable? Of a 2009 result, on 2026 Linux images, at 4K: a workshop paper at best. What would make the VM result non-trivial:

- 16K and 128K record sizes. ext4 does not align file starts to 16K, so aligned dedup at the ZFS zvol default volblocksize (16K since 2.2) should fall well below 90%. The quotable statement is "at the granularity operators actually run, aligned catches X%, not 90%," which is a real dm-vdo (4K) vs ZFS (16K) argument. I could not open Jin & Miller's per-chunk-size tables (their PDF host refused the connection); check whether they already have this.
- Encrypted guests. BitLocker is on by default on Windows 11 and Ubuntu offers LUKS at install. Cross-lineage dedup on those fleets is zero by construction. Cheap to state, and decisive for a whole fleet class.
- Guest-side compression. btrfs zstd guests, NTFS compression, qcow2 compression. All break both alignment and identity.
- Windows guests. WinSxS uses hardlinks and delta compression. Much less predictable than apt.
- Discard on vs off. The "unallocated" category depends entirely on whether the guest issued TRIM. That is a confound that has to be controlled, not a result.

## 2. Is time since clone recoverable on real fleets?

Mostly not, and the corpus list is weaker than it looks.

- Proxmox. Linked clones record the base volume in the VM config. Full clones record nothing; a forum answer from Proxmox staff puts it as "literally a full byte copy" (https://forum.proxmox.com/threads/how-to-determine-the-parent-container-of-a-linked-clone.45331/). Linked clone is offered only from templates and only on storage that supports it. No clone date field anywhere; you would fall back to volume creation time.
- libvirt/qcow2. The backing chain in the qcow2 header is the parent. No date. One `virsh blockpull` or `qemu-img convert` and it is gone.
- VMware. Linked clones keep the parent in the vmdk descriptor; full clones keep nothing. vCenter events record the clone task but age out.
- CloudLab image library. These are bare-metal Frisbee images, not VM disks. They do record a parent image and creation time, but each is a one-shot snapshot of an experiment node. Useful for lineage, useless for a 12-month drift axis.
- Where clones are actually universal. OpenStack or Proxmox on Ceph RBD, where every VM is `rbd clone` of the image snapshot and `rbd info` shows the parent and creation time, and VDI (Horizon instant clones, Citrix MCS). A university cloud on Ceph is the best candidate and is not on the list.

What "reachable by COW" means for a non-cloned fleet: as defined at line 81 ("relative to a declared ancestor"), zero. That makes H1 trivially true for any fleet installed from ISO. The operator's real question is counterfactual: "had I cloned from the base image, what would I have gotten free, versus what dedup gets." The ancestor has to be reconstructed (the dated cloud image or installer of that release), not read from metadata. That is doable for Debian and Ubuntu and is a better design than depending on donor metadata.

The curve in section 5.2, "image age since its recorded clone or install, aggregated across images," is a cross-section: old VMs versus young VMs at one instant. Old VMs are different VMs (long-lived servers) from young ones (test boxes). That is not "cross-lineage share grows with time," it is "old VMs differ from new ones." A reviewer kills the H1 curve on this unless the data is longitudinal, which means the donor kept snapshots or you synthesize it from dated package sets.

## 3. Donor protocol feasibility

"Needs no root" (line 73) is false for every real donor. On Proxmox with ZFS or LVM-thin, on Ceph, on VMFS, images are block devices or RBD objects, not files in a directory. Reading them means root and a consistent snapshot of running VMs. A university IT department will not run a third-party Rust binary as root on production hypervisors within six weeks; security review and change control alone take longer. They will also ask why a per-fleet table is safe when A6 concedes hashes fingerprint installed package versions. The realistic donor set is the author's machines, a homelab or two, and maybe a research lab's Proxmox box where a grad student holds root. The spec should say so.

Compute cost is fine: BLAKE3 and FastCDC each run at multiple GB/s per core, so the bound is reading images, and a nightly window covers single-digit TB. The output is the problem. "One ndjson row per chunk" at 4K on 10 TB is 2.5 billion rows, around 250 GB of ndjson the donor must hold and you must then process in Python. The cross-image match is a hash table with 2.5 billion keys, 80 GB and up. The spec says this "runs on a donor's laptop." It does not. Size it with a sort-merge or per-image sketches and state the memory formula.

## 4. Phase 2 metrics

Transfer is broken as a claimed benefit. None of the four backends reduce bytes moved between hosts. OpenZFS deprecated deduplicated send in 2.0; `zfs send -D` now prints a warning and emits a normal stream (https://openzfs.github.io/openzfs-docs/man/8/zfs-send.8.html, https://github.com/openzfs/zfs/issues/7887). dm-vdo has no replication. XFS reflinks do not survive rsync. So section 2's "sync, migration, and provisioning move unique bytes only" holds for casync, restic and Xet and for nothing in the phase 2 table. What is actually measured, "device bytes written during fleet replay," is write amplification under another name, and for R3 it equals R0 plus the dedup pass by definition. Drop transfer from phase 2, or admit it is a CDC-store benefit and move it to phase 3's motivation.

Cache. Keep it as a paragraph, not a column. The spec already predicts R0 = R2 = R3. Sharper: with cache=none, which A4's O_DIRECT arm implies, the host page cache is out of the picture on three backends, so the metric is moot there. And on XFS even reflink clones, the lineage tier, do not share page cache, because it is keyed per inode. ARC is the only host cache in the set that shares anything at all. That is one quotable operator fact.

Latency. I could not find a controlled p99 comparison of dm-vdo against ZFS fast dedup in print. Red Hat's VDO guide tells you to watch p99 and p99.9 on 4K random write because of index lookups and publishes no numbers (https://docs.redhat.com/en/documentation/red_hat_enterprise_linux/8/html/deduplicating_and_compressing_storage/testing-vdo-performance_deduplicating-and-compressing-storage). A 2026 howto quotes "typical" 20-50% p99 write latency increases with no hardware or measurement behind them (https://oneuptime.com/blog/post/2026-03-04-benchmark-vdo-deduplication-compression-rhel-9/view). The despairlabs post has zdb -S ratios, not latency. So the table has value, but it concentrates in R1 versus R2 at matched granularity under the fleet replay, with DDT and index memory beside it. fio 4K QD1 on vdo versus XFS is an afternoon's work anyone can repeat from Red Hat's own guide.

R3 is not "nothing on the write path." After duperemove shares extents, every guest write into a shared extent triggers XFS copy-on-write: allocate, copy, update the refcount btree. Post-dedup write latency on R3 is the honest price of the "free" option and the spec does not plan to measure it. Also, duperemove defaults to 128K blocks (https://man.archlinux.org/man/extra/duperemove/duperemove.8.en); at 4K on a multi-TB image it produces tens of millions of extents, which is where XFS extent trees fall over. Pick the block size in the spec, and note the census's 4K aligned figure will not match what duperemove at 128K captures.

## 5. The framing objection

"Copy-on-write cannot reach cross-lineage duplicates" is the definition of what a dedup table is for. It has been the motivation paragraph of every dedup paper since Venti. Reviewer sentence one: "we know." Sentence two: the ZFS community already answers "is dedup worth it on my pool" with `zdb -S`, and the despairlabs post the spec cites does exactly that, then recommends block cloning for the copy case and dedup only when "clients can't or won't give direct 'copy me!' signal" (https://despairlabs.com/blog/posts/2024-10-27-openzfs-dedup-is-good-dont-use-it/). So line 31's claim that community advice is naive about the clone share is wrong. The community's position is already "clones or BRT for lineage, and dedup rarely pays for the rest."

The gap ("nobody subtracted the clone share") is real but narrow. The audience is ZFS, Proxmox and VDI operators choosing among clones-only, clones plus dedup, and a chunk store. That is not a FAST audience. What would interest a FAST reviewer is the thing hiding inside H1: the half-life of a clone. How fast does COW sharing decay under real update cadences, per distro, and does it decay to a floor (kernel, firmware, /usr/share) or to zero? Nobody has that curve, and it bears on snapshot retention, template rebuild cadence and the whole golden-image practice. Retitle around it and the paper has a reason to exist. What readers would do differently: rebuild templates every N months instead of enabling dedup, pick a volblocksize, and skip all of it on encrypted fleets.

## 6. Cheaper ways

Yes, and they should be phase 0, run before any pipeline is written.

- `zdb -S` on a pool holding clones versus the same images imported independently gives aligned-at-recordsize totals in one command (https://openzfs.github.io/openzfs-docs/man/master/8/zdb.8.html). I believe pool traversal visits each clone-shared block once via birth-time pruning, the same reason scrub does not rescan shared blocks, which would make zdb -S on the cloned pool nearly "aligned cross-lineage" directly. Unverified. A five-minute test settles it: clone twice, write the same file into both, run zdb -S. If it holds, the ZFS aligned tier is ten minutes per fleet and the donor runs it themselves with no new binary.
- `duperemove --dry-run` and bees stats give the same on XFS and btrfs.
- Dated public corpora. Ubuntu publishes cloud images daily for every release and keeps them for years, so "the same image N months apart" is downloadable and dated. snapshot.debian.org lets you build "installed at T0, upgraded at T1" exactly. Vagrant boxes, NixOS across releases and Docker Hub cover the rest. This is a better time axis than any donor can give, because the cross-section confounds in item 2 vanish.

The one thing the cheap route cannot give is the unaligned ceiling. That is where the pipeline earns its keep. Scope it to that.

## 7. Is 110 hours realistic?

No. Phases 1 and 2 as written are 200 to 250 hours. Where it blows up, in order:

- Weeks 1 to 2, 16 hours, for "a few thousand lines" of Rust plus synthetic controls, corpus scripts and a donor protocol. That is a 60-hour job for someone who has built a CDC pipeline before, and the memory problem from item 3 has to be solved inside it.
- Weeks 3 to 6 are gated on other people's calendars. Six weeks from first ask to second fleet done assumes a yes within days.
- Phase 2 wall time. Four backends plus 4K/16K volblocksize arms, compression arms and an O_DIRECT arm is 8 to 10 configurations. Each runs five fio jobs, a kernel build, three boot storm sizes and a fleet replay, five times, with a fresh pool per rep. At roughly two hours per configuration-rep that is 80 to 100 hours of wall time against CloudLab reservations that expire (16 hours by default, extensions on request) on a popular node type. Every expiry loses a fresh-pool run. Setup adds around 20 hours: dm-vdo needs a 6.9+ kernel, which CloudLab's Ubuntu images do not ship, and OpenZFS 2.3 with blake3 and fast_dedup is a source build.
- Weeks 13 to 14, 16 hours, for curves, a four-backend table with variance, and a reproducibility pack. That is 30 hours minimum.

At 110 hours the honest plan is: cheap-route phase 1 from zdb -S and dated public corpora, the pipeline scoped to the unaligned ceiling, and phase 2 cut to R0, R1, R2 at one volblocksize with fio and fleet replay only. Section 8.4 keeps R1 over R2, but R2 is the cleaner comparison by the spec's own argument in 6.2. Cut R1's second arm before cutting R2.

## 8. Things a reviewer would quote

- Line 135 vs 141. "R0, R2, and R3 share one XFS filesystem on the dedicated NVMe device," but R2 is "Raw file on XFS over dm-vdo." XFS on a vdo device is a different filesystem instance on a different block device. Line 203 repeats the claim.
- Line 139. "Raw file on a ZFS zvol." A zvol is a block device. Either QEMU opens it directly and there is no file, or a filesystem sits on it and the spec must say which, at which point R1 is a two-filesystem stack the spec later says it avoids.
- Line 139. "Whether OpenZFS direct IO applies to a zvol with dedup on is checked and recorded." Already known: direct IO supports neither zvols nor dedup (https://github.com/openzfs/zfs/pull/10018, https://klarasystems.com/articles/managing-cache-and-direct-io-for-databases-on-zfs/). O_DIRECT on /dev/zd0 skips the block-device buffer cache and ARC remains. R1's O_DIRECT arm is not comparable to the others, and the section 6.4 cache bound is ARC-only on R1.
- Line 81. "What siblings actually share depends on when the snapshots were taken." Wrong. Sibling clones share only what was in the origin snapshot at clone time. Later snapshots on each clone never create sharing between siblings. The variable that matters is clone date against a template that is itself being updated, and the ancestry file already has that field. Snapshot cadence belongs to one image's own history, not the fleet split.
- Lines 83 and 89. Category 3 as "found only by CDC" with CDC at 16K mean and aligned at 4K. See item 1.
- Lines 73 and 89. "Needs no root and no guest," but chunking includes "whole file," which requires parsing the guest filesystem, and "zero or unallocated" needs the guest allocation map or it is just "zero." If zero-only, deleted-file remnants in unallocated space inflate every category, which Jin & Miller flagged in 2009.
- Line 41. "Sync, migration, and provisioning move unique bytes only." Not on any phase 2 backend. See item 4.
- Line 143. "Nothing on the write path" and "fixed-block" with no block size. See item 4.
- Line 31. The despairlabs characterization. See item 5.
- Line 97. The cross-section problem. See item 2.
- Line 119. "Runs on a donor's laptop." See item 3.
- Line 203. "XFS is required for hole punching with extent-based allocation." ext4 punches holes too. Prefer, do not require.
- A7 against R1. ZFS with compression off does not elide zero blocks. With dedup on, every zero block collapses onto one DDT entry with a refcount in the millions, a known pathological hot spot. The census excludes zeros; R1 will store and dedup them. Either set zle on R1's non-compression arm or discard before measuring.
- Line 242. CloudLab c6525-100g checks out: 24-core EPYC 7402P, 128 GB, two 1.6 TB PCIe 4 NVMe, 25G and 100G ConnectX-5 (https://docs.cloudlab.us/hardware.html).
- Line 53. DeDe checks out: 113 VMs, 1.3 TB, 80% (https://www.usenix.org/conference/usenix-09/decentralized-deduplication-san-cluster-file-systems).

## Bottom line

There is a paper in the clone half-life curve if it is built on dated public images and validated against one or two real fleets. There is no paper in confirming H2 on Linux guests at 4K. Fix the category 3 definition, drop transfer from phase 2, cut phase 2 to what fits the hours, and the design is worth doing.

## Sources

- Jin & Miller, SYSTOR '09 abstract: https://ssrc.us/pub/jin-systor09.html
- Proxmox forum on linked clone parents: https://forum.proxmox.com/threads/how-to-determine-the-parent-container-of-a-linked-clone.45331/
- OpenZFS zfs-send.8: https://openzfs.github.io/openzfs-docs/man/8/zfs-send.8.html
- OpenZFS issue #7887 (deprecate dedup send): https://github.com/openzfs/zfs/issues/7887
- OpenZFS direct IO PR #10018: https://github.com/openzfs/zfs/pull/10018
- Klara on direct IO: https://klarasystems.com/articles/managing-cache-and-direct-io-for-databases-on-zfs/
- Red Hat VDO performance testing: https://docs.redhat.com/en/documentation/red_hat_enterprise_linux/8/html/deduplicating_and_compressing_storage/testing-vdo-performance_deduplicating-and-compressing-storage
- oneuptime VDO howto: https://oneuptime.com/blog/post/2026-03-04-benchmark-vdo-deduplication-compression-rhel-9/view
- despairlabs on OpenZFS dedup: https://despairlabs.com/blog/posts/2024-10-27-openzfs-dedup-is-good-dont-use-it/
- zdb.8: https://openzfs.github.io/openzfs-docs/man/master/8/zdb.8.html
- duperemove.8: https://man.archlinux.org/man/extra/duperemove/duperemove.8.en
- CloudLab hardware: https://docs.cloudlab.us/hardware.html
- DeDe, USENIX ATC '09: https://www.usenix.org/conference/usenix-09/decentralized-deduplication-san-cluster-file-systems
