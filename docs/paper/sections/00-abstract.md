# Abstract

Sandbox that run an agentic coding tasks usually start from a prebuilt disk/os image. 
Operators report that a typical images are used only by a single sandbox. To reduce boot latency, they have stopped caching images per host and place sandboxes by load instead.
We captured the disks of N sandboxes from RL environments, coding-agent sessions and CI jobs, and found that certain storage blocks are not unique per host.
X% of the bytes an arbitrary task writes already exist on the disk of another arbitrary vm.
Deduplication on each host can only share these bytes when both sandboxes boot on the same host.
This onlyhappens Y% of the time given a load-based placement stratergy.
A central content-addressed store shared by all hosts shares the remaining Z% of bytes.
When measured against ZFS deduplication on a single host, the shares bytes are measured at A.
Beyond H hosts, when placed by load, sharing is worth that cost. Below H, per-host deduplication is measured as economically viable.
We release the captured corpus.
