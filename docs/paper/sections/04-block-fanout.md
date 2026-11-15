# 4 Block Fanout

The *block fanout* of a chunk is the number of VMs whose disks contain
it. A chunk a task writes is a *duplicate* if an earlier VM, sharing no
lineage with it, already contains the same chunk; otherwise it is *unique*.

**How much is duplicated.** [TBD: fraction of written bytes that are
duplicates, per workload, as a distribution over tasks. Pilot: coding tasks
10.2 of 15.1 GB (68%); CI 3.0 of 12.8 GB (24%).] [pilot]

**Fanout distribution.** [TBD: CDF of block fanout per workload, beside the
published image fanout.]

**What the duplicates are.** [TBD: duplicate bytes by path category. Pilot:
for coding tasks, 9.5 of 12.4 GB written to dependency caches duplicate an
earlier task; for CI, build output (4.7 GB) is almost all unique.] [pilot]

**Within and across repositories.** [TBD. Pilot: 12.1 of 13.2 GB of duplicates
are within one repository.] [pilot]

**Chunking.** [TBD: fixed 4 KiB, fixed 16 KiB and CDC. Pilot: 13.2, 11.8 and
14.3 GB duplicated.] [pilot]

**Simpler alternatives.** Much duplicate content is package data, which a
shared package cache or one image per repository would also share. [TBD: bytes
each alternative leaves duplicated, on the same corpus.]

**Summary.** [TBD: one sentence.]
