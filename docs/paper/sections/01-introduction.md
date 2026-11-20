# 1 Introduction

Each task an AI agent runs gets its own environment image. In 2024, SWE-Gym
shipped one image per task, 2,438 images and 6 TB in total [@swe-gym]. Moonshot
reports creating tens of thousands of VMs within seconds, each with a unique
set of images [@kimi-k3]. In one week of early 2026, DeepSeek's platform held
more than 130 TB of environment images, and its microVMs alone used two base
images and 53,590 task-specific disks [@dsec]. Each of DeepSeek's VMs reads
only 4.2% to 13.3% of its image, and loading whole images before start made a
burst of VMs take 1.71 times as long [@dsec]. Alibaba reports that when
environments time out, resetting them takes 78% of rollout time, with image
downloads saturating the network [@rollart].

Some operators decide where a VM runs by what the host already holds.
Cloudflare favors hosts that have the VM's image or snapshot
[@cloudflare-sandboxes]. Prime Intellect schedules VMs where their environment
is already present [@prime-sandboxes]. Kubernetes scores nodes by the images
they hold [@k8s-imagelocality]. Other operators place by load and fetch what is
missing. DeepSeek samples a few hosts and picks the least loaded [@dsec].
Sandman spreads VMs across hosts, and reports that concentrating them on hosts
with the image cached caused failures to accumulate [@sandman].

Under either policy, a host reuses disk data only when it already holds the
exact image. Placing by image helps only when the same image is used again, and
DeepSeek measures that a microVM image is typically used by one VM within a job
[@dsec]. Placing by load gives up on reuse at the host.

Sharing below the image has been reported only for files. DeepSeek builds
container environments from shared layers and serves the files accessed by 70%
of its container tasks from one 30 TB deduplicated set [@dsec]. A VM's disk is
a block device holding a file system, written to as the task runs, and DeepSeek
stores each microVM workspace as its own disk image [@dsec]. For VM disks we
found no report of how many blocks two different VMs have in common.

A *task* is one unit of agent work run in one VM, which may be an RL episode, a
coding-agent session or a CI job. We ask three questions.

1. **Block fanout.** How much of what a task writes, and of what it reads
   before its first command, is identical to blocks on the disk of a VM it was
   not copied from? (§4)
2. **Placement.** How many of those duplicate blocks are already on the host,
   under placement by load, by image and by repository? (§5)
3. **Cost.** What does it cost to share them, with deduplication on each host
   or with one content-addressed store across all hosts? (§7, §8)

A content-addressed store names each block by its fingerprint (hash). A host
can then recognise blocks it already holds under another image via the image's
manifest, the list of its block fingerprints, and fetch only the rest. Fetching
on demand is established practice [@lambda, @dsec], and it is the baseline we
measure against.

We answer the questions on the captured disks of [TBD: N] VMs from three
workloads and on [TBD: 2 or 4] bare-metal hosts.

**Findings.** [TBD: one sentence per question, written when the data exists.]

**Contributions.**

- A released corpus of VM disks from three agent workloads, captured at block
  level with file attribution.
- A measurement of block fanout across VMs that share no snapshot.
- The split of duplicate blocks between the same host and other hosts under
  each placement policy, with the load imbalance each causes.
- A matched comparison of per-host ZFS deduplication and a fleet-wide
  content-addressed store.

The content-addressed store is the system under test, and we do not claim its
mechanism, which is not new (§9). The store is a minimal implementation built
to measure bytes stored and bytes sent. §6 lists the known techniques the store
omits, and each slow number in §7 is reported with its cause.
