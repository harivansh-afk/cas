# 9 Related Work

**Content-addressed storage and deduplication.** Venti addresses blocks by
fingerprint [@venti]; LBFS adds content-defined chunking [@lbfs]. Deduplication
of primary data and of VM images has been measured at scale [@meyer;
@elshimi; @jin-miller; @jayaram]. Those studies analyse stored images, most
sharing an operating-system lineage. We measure blocks written by independent
tasks.

**Moving VM state on demand.** Fetching a VM's disk on demand at another host,
and skipping data the destination already holds, dates to
[@sapuntzakis; @isr; @snowflock]. AWS Lambda loads container images on demand
from a fleet-wide content-addressed store [@lambda]. We reuse these mechanisms
and claim none.

**Placement and sharing.** Co-locating VMs to raise host-local sharing was
studied for memory [@memory-buddies; @sindelar] and for disk blocks [@smio].
Routing data among deduplicating nodes trades deduplication against skew
[@dong]. We apply the same trade to VM disks and add the fleet-store
alternative on the same axes.

**VM infrastructure.** DSec reports image fanout and places by load
[@dsec]; Sandman rejects placement by image [@sandman]. Reports on SWE-bench
image sizes work at layer or file level [@epoch-swebench; @logicstar]. We add
the block level and the placement analysis.

**Comparing deduplicating stores.** DupHunter compares ZFS and Ceph
deduplication on Docker registry layers, with unmatched block sizes
[@duphunter]. We compare on VM disks at equal chunk size.

[check: Sun et al. (ACM TOS 2018) and Graviet et al. (2026) are unread and may
narrow the placement claim.]
