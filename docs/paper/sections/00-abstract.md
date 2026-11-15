# Abstract

AI agents, reinforcement learning environments (RL) and continuous integration pipelines (CI) often run independent tasks in isolated virtual machines (VMs), booting their own disk images.
Published deployments keep tens of thousands of such images, terabytes to over a hundred terabytes in total, many built on a handful of base images.
Operators reuse disk data by image identity, not by content.
A host skips fetching only when it already holds that exact image.
Some place a VM by image locality, while others place by load and accept the cost of fetching.
We ask how much content distinct images share at the block level.
We captured the disks of [N] VMs from RL environments, coding-agent sessions and CI jobs, comparing them block by block.
[X%] of the bytes a task writes, and [R%] of the blocks it reads before its first command, are identical to blocks on the disk of a VM it was not copied from.
A host can use those blocks only if it holds them, which is the case for [Y₁%] to [Y₂%] depending on placement.
A content-addressed store names each block by its fingerprint, so a host recognises the blocks it already holds under another image and fetches only the rest.
We measure such a store against ZFS deduplication on each host, on the same machines.
[Result.] We release the corpus.
