# 1 Introduction

AI agents do their work inside VMs. A *task* is one unit of agent work: a
reinforcement-learning (RL) episode, a coding-agent session or a CI job. A
*VM* is a Linux virtual machine with its own virtual disk; each runs one
task. Each VM starts from an *image* holding the task's repository, toolchain and
dependencies. One operator reports more than 130 TB of images active in a
single week [@dsec].

These images are rarely reused. The same operator measures *image fanout*, the
number of VMs that use one image within one job, at a median of one for microVM
images and three for container images [@dsec]. A host-local image cache
therefore seldom hits, and the operator places VMs by load alone. A
second operator states that it does not place VMs by image, because
packing them concentrated failures on few hosts [@sandman].

An image is a coarse unit of identity. Two tasks on one repository install the
same packages into different images. Neither disk descends from the other, so
snapshots and clones cannot share the result. The blocks may still be equal.
Published measurements of VM images work at the level of layers or files
[@epoch-swebench; @logicstar; @dsec]; we know of none at the level of blocks
written by independent VMs.

This paper asks three questions.

1. **Block fanout.** How much of what a task writes already exists on the disk
   of a VM with which it shares no lineage? (§4)
2. **Placement.** How many of those duplicate chunks are on the same host,
   under placement policies from load-based to affinity? (§5)
3. **Cost.** What does it cost to share them: with deduplication on each host,
   or with one content-addressed store across the fleet? (§7, §8)

We answer them on the captured disks of [TBD: N] VMs and on
[TBD: 2 or 4] bare-metal hosts.

**Findings.**

- [TBD: block fanout. Pilot, 25 disks: 10.2 of 15.1 GB (68%) of coding-task
  bytes and 3.0 of 12.8 GB (24%) of CI bytes duplicate an earlier task.] [pilot]
- [TBD: placement. Pilot: 12.1 of 13.2 GB of duplicates are same-host under
  repository affinity; 2.3 of 13.2 GB under random placement on 16 simulated
  hosts.] [pilot]
- [TBD: cost on one host against ZFS deduplication.]
- [TBD: cost across hosts and the crossover.]

**Contributions.**

- A corpus of VM disks from three agent workloads, captured at block
  level with file attribution, and released.
- The first block-fanout measurement of these workloads that we are aware of.
  [check: wording after the related-work pass]
- The same-host and other-host split of duplicate chunks across placement
  policies, with the load imbalance each policy causes.
- A matched comparison of per-host ZFS deduplication and a fleet-wide
  content-addressed block store, and the conditions under which each wins.

The content-addressed store is the system under test. Its mechanism is not new
(§9) and we do not claim it.
