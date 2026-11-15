# Abstract

AI agents, reinforcements learnings environments (RL) and continous integration pipelines (CI) often run independent tasks in isolated virtual machines (VMs), booting their own disk images.
Published deployments keep tens of thousands of such images, terabytes to over a hundred terabytes in totality, many built on a handful of base images.
Operators often treat images as the unit of sharing, placing VMs on hosts based on image locality, or based on host-load/capacity.
We question in this paper how much these disks have in common below (what to put here?). 
We captured the disks of [N] VMs from RL environments, coding-agent sessions and CI jobs, comparing them block by block.
[X%] of the bytes a task writes are identical to bytes on the disk of a VM it was not copied from. 
A host can deduplicate those bytes only if both VMs ran on it, which holds for [Y₁%] to [Y₂%] of them depending on placement. We measure the alternative, a content-addressed store shared by all hosts, against ZFS deduplication on the same machines. 
[Result.] We release the corpus.
