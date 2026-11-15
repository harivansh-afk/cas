# Abstract

AI agents, reinforcement learning environments (RL) and continuous integration pipelines (CI) often run independent tasks in isolated virtual machines (VMs), booting their own disk images.
Published deployments keep tens of thousands of such images, terabytes to over a hundred terabytes in total, many built on a handful of base images.
Operators reuse disk data by image identity, not by content.
A host skips fetching only when it already holds that exact image.
Some place a VM by image locality, while others place by load and accept the cost of fetching.
We ask how much content distinct images share at the block level.
We captured the disks of [N] VMs from RL environments, coding-agent sessions and CI jobs, comparing them block by block.
[X%] of the bytes a task writes are identical to bytes on the disk of a VM it was not copied from.
A host can deduplicate those bytes only if both VMs ran on it, which holds for [Y₁%] to [Y₂%] of them depending on placement. We measure the alternative, a content-addressed store shared by all hosts, against ZFS deduplication on the same machines.
[Result.] We release the corpus.
