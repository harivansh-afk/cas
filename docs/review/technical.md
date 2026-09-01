# Verification of technical claims in docs/spec.md (sections 1, 6, 7, 8.1)

Date: 2026-09-01. Each item: verdict, what the source says, URLs.

## 1. Page cache is per inode; reflinked files hold separate pages

CONFIRMED. Dave Chinner at LSF/MM 2018: the page cache is indexed by (inode, offset) while shared extents are known only by physical block, so with 500 containers on one image "you have 500 copies of /bin/bash in memory". A block-indexed buffer cache was proposed; Matthew Wilcox said a solution was coming "maybe next week". Nothing merged since. A 2016 thread (Chinner, Szeredi) discussed XFS-only page sharing for reflinked inodes and went nowhere. The only page-cache-sharing work in flight is EROFS-specific (RFC v5, Jan 2025), not reflink.

- https://lwn.net/Articles/747633/
- https://www.spinics.net/lists/linux-btrfs/msg55424.html
- https://lkml.iu.edu/hypermail/linux/kernel/2501.0/02808.html

## 2. dm-vdo has no read cache

CONFIRMED for mainline. The kernel admin guide describes only the block map cache (metadata) and the UDS deduplication index. The dm table has no read-cache parameter. Required parameters: offset, logical device size, storage device, storage device size, minimum IO size, block map cache size, block map era length. Optional: ack, bio, bioRotationInterval, cpu, hash, logical, physical, maxDiscard, deduplication, compression. The old RHEL 7 VDO manager had `--readCache` and `--readCacheSize`, "a second cache ... for caching data blocks read from the storage system to verify VDO's deduplication advice", off by default. Red Hat removed readcache/readcachesize as unsupported on RHEL 8 VDO.

- https://docs.kernel.org/admin-guide/device-mapper/vdo.html
- https://docs.redhat.com/en/documentation/red_hat_enterprise_linux/7/html/storage_administration_guide/vdo-ig-tuning-vdo
- https://docs.redhat.com/documentation/en-us/red_hat_hyperconverged_infrastructure_for_virtualization/1.8/html/1.8_release_notes/bug-fixes-180

## 3. ZFS ARC is keyed by physical block

CONFIRMED. arc.c: `buf_hash(spa, dva, birth)` = `cityhash4(spa, dva->dva_word[0], dva->dva_word[1], birth)`; `HDR_EQUAL` compares dva, birth and spa. Deduplicated blocks share one DVA, so they share one ARC header.

- https://github.com/openzfs/zfs/blob/master/module/zfs/arc.c

## 4. zvol dedup granularity is volblocksize

CONFIRMED. OpenZFS Workload Tuning: "Each entry in the hash table is a record of a unique block in the pool. (Where the block size is set by the recordsize or volblocksize properties.)"

- https://openzfs.github.io/openzfs-docs/Performance%20and%20Tuning/Workload%20Tuning.html

## 5. OpenZFS 2.3 fast dedup names and release

CONFIRMED. zfs-2.3.0 released 2025-01-14 (GitHub release page lists Fast Dedup #15896 and Direct IO #10018). `feature@fast_dedup`, GUID `com.klarasystems:fast_dedup`, read-only compatible. `dedup_table_quota` is a pool property and "works for both legacy and fast dedup tables". `zpool ddtprune -d days | -p percentage pool` "prunes older unique entries from the dedup table". "Sorted flush" is a fair paraphrase: log entries live in AVL trees keyed by ddt_key (ddt_log.c), and ddt.c notes that "sequential log flush usually combines many entries per leaf". Two logs, one appending and one flushing, swap every `zfs_dedup_log_txg_max` txgs (zfs.4).

- https://github.com/openzfs/zfs/releases/tag/zfs-2.3.0
- https://openzfs.github.io/openzfs-docs/man/master/7/zpool-features.7.html
- https://openzfs.github.io/openzfs-docs/man/master/7/zpoolprops.7.html
- https://openzfs.github.io/openzfs-docs/man/master/8/zpool-ddtprune.8.html
- https://openzfs.github.io/openzfs-docs/man/master/4/zfs.4.html
- https://github.com/openzfs/zfs/blob/master/module/zfs/ddt_log.c

## 6. Direct IO on zvols and with dedup

RESOLVED: neither applies. zfsprops(7), `direct` property: "Currently Direct I/O is not supported with zvols. If dedup is enabled on a dataset, Direct I/O writes will not check for deduplication. Deduplication and Direct I/O writes are currently incompatible." PR #10018 (merged 2024-09-13): "ZVOLs and dedup is not currently supported with Direct I/O." The spec's "checked and recorded before the O_DIRECT run" has a documented answer: on R1 the direct property does nothing, and the guest's O_DIRECT reaches the zvol as ordinary ARC-backed IO.

- https://openzfs.github.io/openzfs-docs/man/master/7/zfsprops.7.html
- https://github.com/openzfs/zfs/pull/10018

## 7. checksum=blake3 with dedup=on

PARTLY. The R1 configuration is wrong as written. blake3 is a valid checksum and carries `ZCHECKSUM_FLAG_DEDUP` (zio_checksum.c), so dedup can use it. But `dedup=on` does not pick up the checksum property. `dedup_changed_cb` (dmu_objset.c) calls `zio_checksum_dedup_select`, which for `ZIO_CHECKSUM_ON` returns `spa_dedup_checksum(spa)`; `ddt_create` sets that to `ZIO_DEDUPCHECKSUM`, defined as `ZIO_CHECKSUM_SHA256` (include/sys/zio.h line 115). `dmu_write_policy` then uses the dedup checksum for data blocks, overriding `checksum=blake3`. zfsprops(7): "The default deduplication checksum is sha256 (this may change in the future). When dedup is enabled, the checksum defined here overrides the checksum property." To dedup with BLAKE3, set `dedup=blake3` with `feature@blake3` enabled. `checksum=blake3` is then redundant for data blocks.

- https://github.com/openzfs/zfs/blob/master/module/zfs/dmu_objset.c
- https://github.com/openzfs/zfs/blob/master/module/zfs/zio_checksum.c
- https://github.com/openzfs/zfs/blob/master/module/zfs/ddt.c
- https://github.com/openzfs/zfs/blob/master/include/sys/zio.h
- https://github.com/openzfs/zfs/blob/master/module/zfs/dmu.c
- https://openzfs.github.io/openzfs-docs/man/master/7/zfsprops.7.html

## 8. duperemove on XFS

PARTLY. FIDEDUPERANGE on XFS was merged for Linux 4.9 (Darrick Wong series, "for-dave-for-4.9") and the EXPERIMENTAL tag was removed in 4.16 (Jan 2018 pull: "Remove EXPERIMENTAL tag from reflink!"). duperemove man page: "Deduplication is currently only supported by the btrfs and xfs filesystem." `-b`: "Use the specified block size for reading file extents. Defaults to 128K." Source bounds: `MIN_BLOCKSIZE` 4K, `MAX_BLOCKSIZE` 1M (file_scan.h), so 4K is accepted. Matching is extent-level by default: "if there are two files which are logically identical but are laid out on disk with different extent structure they won't be deduped." Per-block hashes are optional, and `--dedupe-options=partial` ("comparing portions of extents to each other", CPU intensive, off by default, "semantics of the partial argument may change") is what produces block-level matches. R3 as fixed-block dedup therefore needs `-b 4k` plus `--dedupe-options=partial`, and the spec should say so.

- https://lwn.net/Articles/702633/
- https://lkml.iu.edu/hypermail/linux/kernel/1801.3/05584.html
- https://manpages.debian.org/unstable/duperemove/duperemove.8.en.html
- https://github.com/markfasheh/duperemove/blob/master/file_scan.h

## 9. FALLOC_FL_PUNCH_HOLE on XFS with O_DIRECT files and io_uring

CONFIRMED for each component; the exact combination is not documented anywhere found. fallocate(2): PUNCH_HOLE supported on XFS since Linux 2.6.38. io_uring_enter(2): `IORING_OP_FALLOCATE` available since 5.6. No source restricts hole punching by the file's open flags.

- https://man7.org/linux/man-pages/man2/fallocate.2.html
- https://man7.org/linux/man-pages/man2/io_uring_enter.2.html

## 10. QEMU vhost-user-blk front end and qemu-storage-daemon export

CONFIRMED. `hw/virtio/vhost-user-blk-pci.c` is in the QEMU tree. The vhost-user device docs list vhost-user-blk with qemu-storage-daemon as its backend. Guest side: `-chardev socket,id=c0,path=<sock> -device vhost-user-blk-pci,chardev=c0` plus a shared memory backend (`-object memory-backend-memfd,id=mem,size=...,share=on -numa node,memdev=mem`). Export syntax:

```
--export type=vhost-user-blk,id=<id>,node-name=<node>,addr.type=unix,addr.path=<sock>[,writable=on|off][,logical-block-size=<n>][,num-queues=<n>]
```

The doc example exports qcow2. A raw file is `--blockdev driver=file,node-name=f,filename=disk.raw --blockdev driver=raw,node-name=r,file=f` with `node-name=r` in the export.

- https://www.qemu.org/docs/master/tools/qemu-storage-daemon.html
- https://www.qemu.org/docs/master/system/devices/virtio/vhost-user.html
- https://github.com/qemu/qemu/blob/master/hw/virtio/vhost-user-blk-pci.c

## 11. Crates and licenses

PARTLY. The crates exist; the license column in section 7.3 is imprecise.

| Crate | Version | License |
|---|---|---|
| vhost-user-backend | 0.23.0 | Apache-2.0 |
| vm-memory | 0.17.2 | Apache-2.0 OR BSD-3-Clause |
| virtio-queue | 0.18.0 | Apache-2.0 AND BSD-3-Clause |
| blake3 | 1.8.7 | CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception |
| fastcdc | 5.0.0 (2026-08-22) | MIT |

Cloud Hypervisor has `vhost_user_block/` (depends on vhost-user-backend, virtio-queue, vm-memory); the repo's LICENSES directory holds Apache-2.0 and BSD-3-Clause texts. Suggest the rust-vmm row read "Apache-2.0 / BSD-3-Clause".

- https://crates.io/crates/vhost-user-backend
- https://crates.io/crates/vm-memory
- https://crates.io/crates/virtio-queue
- https://crates.io/crates/blake3
- https://crates.io/crates/fastcdc
- https://github.com/cloud-hypervisor/cloud-hypervisor/tree/main/vhost_user_block
- https://github.com/cloud-hypervisor/cloud-hypervisor/tree/main/LICENSES

## 12. CloudLab c6525-100g and access

Hardware CONFIRMED. CloudLab hardware page: "24-core AMD 7402P at 2.80GHz", "128GB ECC Memory (8x 16 GB 3200MT/s RDIMMs)", "Two 1.6 TB NVMe SSD (PCIe v4.0)", "Dual-port Mellanox ConnectX-5 25 GB NIC (PCIe v4.0) (one port available for experiment use)", "Dual-port Mellanox ConnectX-5 Ex 100 GB NIC (PCIe v4.0) (one port available for experiment use)".

Access PARTLY. Free: "CloudLab will be available, free of charge, to all researchers" (Flux group page); "Registering doesn't cost anything, it's simply for accountability" (manual). The spec's "the sponsor approves the project" is wrong. A project must be started by "a faculty member, senior research staff, or in some other senior position"; "The application will be reviewed by our staff ... The review process may take a few days." The project leader then approves members. The AUP's research test is intent to disseminate findings in scholarly venues.

- https://docs.cloudlab.us/hardware.html
- https://docs.cloudlab.us/users.html
- https://www.flux.utah.edu/project/cloudlab
- https://cloudlab.us/aup

## 13. OVHcloud Advance bare metal 2026, AMD EPYC 4005

CONFIRMED, with detail. Announced 2026-02-09: "AMD EPYC 4005 x86 processors with up to 16 cores/32 threads with DDR5 ECC memory". US price list:

| Model | CPU | Cores | RAM | Storage | From |
|---|---|---|---|---|---|
| Advance-1 2026 | EPYC 4245P | 6c/12t | 32 to 256 GB | 2 x 960 GB NVMe | $204/mo |
| Advance-2 2026 | EPYC 4345P | 8c/16t | 64 to 256 GB | 2 x 960 GB NVMe | $255/mo |
| Advance-3 2026 | EPYC 4465P | 12c/24t | 64 to 256 GB | 2 x 960 GB NVMe | $332/mo |
| Advance-4 2026 | EPYC 4585PX | 16c/32t | 64 to 256 GB | 2 x 960 GB NVMe | $383/mo |

The spec's fallback should name Advance-4 2026 and note 64 GB base RAM.

- https://corporate.ovhcloud.com/en/newsroom/news/ovhcloud-bare-metal-2026-amd/
- https://us.ovhcloud.com/bare-metal/prices/

## 14. Linux 6.9 merged dm-vdo

CONFIRMED. Linux 6.9, released 2024-05-12, "adds a new device mapper VDO (virtual data optimizer) target which provides block-level deduplication, compression, and thin provisioning".

- https://kernelnewbies.org/Linux_6.9

## 15. 4K alignment of ext4 file data inside VM images

CONFIRMED, with the listed exceptions. sfdisk(8): "The default start offset for the first partition is 1 MiB." e2fsprogs mke2fs.conf defaults: `blocksize = 4096`; mke2fs(8) picks usage type `small` (blocksize 1024) for filesystems from 3 MB to under 512 MB and `floppy` (1024) under 3 MB. `inline_data` is not in the default ext4 feature list; when enabled, files under 60 bytes live in the inode (up to about 160 bytes with the xattr area). btrfs inlines small files up to `max_inline`, default `min(2048, page size)`. qcow2: `DEFAULT_CLUSTER_SIZE 65536` in block/qcow2.c; clusters are cluster-aligned in the host file, so 4K alignment survives unless a smaller `cluster_size` (minimum 512) is chosen or clusters are compressed. FAT boot/EFI partitions are outside the rule.

- https://man7.org/linux/man-pages/man8/sfdisk.8.html
- https://github.com/tytso/e2fsprogs/blob/master/misc/mke2fs.conf.in
- https://man7.org/linux/man-pages/man8/mke2fs.8.html
- https://docs.kernel.org/filesystems/ext4/inlinedata.html
- https://btrfs.readthedocs.io/en/latest/ch-mount-options.html
- https://github.com/qemu/qemu/blob/master/block/qcow2.c
- https://www.qemu.org/docs/master/interop/qcow2.html

## 16. Resynchronization rule for CDC over modified extents

PARTLY. The locality property is textbook: LBFS (SOSP 2001) section 3.1, "Insertions and deletions therefore only affect the surrounding chunks"; Figure 1 shows boundaries beyond the edit unchanged. Re-chunking only the changed region from a boundary before it is described in an EMC patent, "Targeted chunking of data" (US10324805B1, granted 2019-06-18): start at the expected chunk boundary before the change, process "the extra sliding window-length of data bytes at the starting and ending boundaries of the changed region" so the chunker has the same state, and stop after the change. The Xet chunking spec states the invariant that makes "stop when the cut re-agrees with the old cut" valid for gear and FastCDC chunkers: hash state resets at each boundary, so once a new boundary coincides with an old one the rest of the cut is identical ("Only reset h when you emit a boundary. This ensures chunking is stable even when streaming input in pieces"). No source found uses the spec's exact wording. Cite LBFS for locality and the EMC patent or Xet spec for the mechanism.

- https://pdos.csail.mit.edu/papers/lbfs:sosp01/lbfs.pdf
- https://patents.google.com/patent/US10324805B1/en
- https://huggingface.co/docs/xet/en/chunking

## Corrections the spec needs

1. Item 7: R1 must set `dedup=blake3`, not `dedup=on`; with `dedup=on` the DDT hashes with SHA256 regardless of `checksum=blake3`.
2. Item 6: Direct IO is unsupported on zvols and incompatible with dedup by documentation. Drop the "checked and recorded" hedge and state it.
3. Item 8: R3 is only block-level with `-b 4k --dedupe-options=partial`; the default is extent-level at 128K.
4. Item 12: CloudLab projects are started by faculty or senior research staff and reviewed by CloudLab staff, not approved by a sponsor.
5. Item 13: name the fallback as Advance-4 2026 (EPYC 4585PX, 16c/32t), 64 GB DDR5 base.
6. Item 11: rust-vmm row license should read "Apache-2.0 / BSD-3-Clause".
