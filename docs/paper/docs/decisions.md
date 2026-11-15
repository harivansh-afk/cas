# Decisions

Dated, one line each. A decision stays until a later line replaces it.

## Direction

- 2026-11-15. The paper is a measurement study. Question: sandbox images are
  almost never reused; are the blocks inside them? If so, what does sharing
  them cost with per-host deduplication against a fleet-wide content-addressed
  store?
- 2026-11-15. The store is the system under test, not the contribution.
- 2026-11-15. Workloads, in order: RL rollout environments, coding-agent
  sessions, CI as the low-sharing contrast.
- 2026-11-15. Comparators: ZFS deduplication per host, raw XFS, one image per
  repository, a shared package cache.
- 2026-11-15. Out of scope and stated: memory state, RDMA, fork fan-out for
  search, restore correctness.

## Claims

Allowed if the data holds:

1. Block fanout of content written by sandboxes that share no lineage, with
   method.
2. The same-host and other-host split of duplicate chunks across placement
   policies, with the load imbalance of each.
3. Per-host ZFS deduplication and the fleet store on common axes, and where
   they cross.

Not allowed:

- Novelty of the mechanism. Prior: Venti, Internet Suspend/Resume, SnowFlock,
  Liquid, TiDedup, AWS Lambda.
- "First to relate placement to host-scoped sharing." Prior: Memory Buddies,
  Sindelar et al., SMIO.
- "First to quantify the balance cost of affinity." Prior: Dong et al.,
  LRScheduler.
- That operators place sandboxes by image. DSec and Sandman place by load.
  Affinity is the hypothetical end of the range.
- Fleet-scale results presented as measured. Placement beyond the physical
  hosts is simulated over captured disks and labelled so.
- Any number from a development run. Pilot numbers are marked `[pilot]` in
  section files and are replaced before submission.

## Writing

- 2026-11-15. Sections are edited as Markdown in `sections/`. `paper.tex` is
  transcribed from them by hand; the Markdown is the source of truth.
- 2026-11-15. Every sentence uses only terms that are common knowledge, defined
  earlier, or defined in that sentence. Terms come from `terminology.md`.
- 2026-11-15. Numbers appear with their denominator. Distributions, not single
  medians.
- 2026-11-15. A limit is stated next to the claim it limits.
- 2026-11-15. Markers in section files: `[TBD: ...]` awaits data, `[pilot]`
  is a pilot value, `[check]` is an unverified statement or citation.

## Evidence

- 2026-11-15. Predictions are frozen before the first node run.
- 2026-11-15. Measured revision of the store: `701f5a1`.
- 2026-11-15. Testbed: 2 x r6615 on 18-21 November (approved); 4 x r6615 on
  22-25 November (requested, pending approval).
