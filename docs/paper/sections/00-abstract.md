# Abstract

AI agents run each task in a sandbox with its own disk image. Operators report
that these images are almost never reused, so host-local image caches miss and
sandboxes are placed by load. We ask whether the blocks inside the images are
reused even when the images are not. We capture the disks of [TBD: N] sandboxes
from RL rollout environments, coding-agent sessions and CI jobs, and measure
block fanout: how many sandboxes contain each chunk. [TBD: X%] of the bytes a
task writes already exist on the disk of a sandbox with which it shares no
lineage. Whether those bytes are on the same host depends on placement:
[TBD: Y%] under affinity placement and [TBD: Z%] under load-based placement.
We then measure what it costs to share them, comparing ZFS deduplication on
each host with a fleet-wide content-addressed block store on the same hardware.
[TBD: one sentence stating where the two cross.] We release the corpus.
