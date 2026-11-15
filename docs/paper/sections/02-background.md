# 2 Background

**Provisioning a VM disk.** A VM needs its image on the host before
it can start. The host either copies the whole image or loads it on demand,
fetching each part when it is first read [@slacker; @lambda]. A *cold start* is
a start on a host that holds none of the image.

**Sharing by lineage.** A *snapshot* is a read-only copy of an image at one
moment. A *clone* is a writable image whose initial contents equal a snapshot
[@openzfs-concepts]. Two disks *share lineage* if one is a clone of a snapshot
of the other, or both are clones of a common snapshot. Snapshots, clones and
image layers share only content that lineage records. Two VMs that
install the same package independently share none.

**Sharing by content.** *Deduplication* stores one copy of equal data. The
store splits data into *chunks*, computes a *fingerprint* (a collision-resistant
hash) of each, and keeps one copy per fingerprint [@venti; @lbfs]. A store whose
chunks are addressed by fingerprint is *content-addressed*. Deduplication finds
equal content regardless of lineage.

**Deduplication on one host.** ZFS deduplicates blocks inline, on the write
path, against a deduplication table [@openzfs-concepts]. The table belongs to
one storage pool, and a pool belongs to one host [check: cite]. Equal blocks on
two hosts are stored twice, and moving a disk between hosts sends them again
[check: `zfs send` behaviour].

**Deduplication across a fleet.** A fleet-wide content-addressed store keeps one
fingerprint namespace for all hosts. AWS Lambda stores container images this
way and loads chunks on demand [@lambda]. Such a store pays for an index, for a
second write when deduplication is post-process, and for a network round trip
when a host reads a chunk it does not hold.

**Placement decides what a host can share.** With per-host deduplication, two
equal chunks are shared only if their VMs run on the same host. Placing
similar virtual machines together to raise host-local sharing is an old idea,
studied for memory pages [@memory-buddies; @sindelar] and for disk blocks
[@smio]. It trades sharing against load balance [@dong]. VM operators today
place by load (§1), which is the opposite end of that trade.
