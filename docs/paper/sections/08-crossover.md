# 8 Per-Host Deduplication or a Fleet Store

Sections 5 and 7 combine into one comparison. For each placement policy we
compute, for per-host ZFS deduplication and for the fleet store:

- bytes stored across the fleet;
- bytes moved to start each sandbox;
- time to first command;
- memory spent on indexes;
- load imbalance.

[TBD: figure. One panel per metric, placement policy on the x-axis, one line
per design.]

**Where they cross.** [TBD: the placement policy and fleet size beyond which
the fleet store stores or moves fewer bytes, and what it pays in latency
there.]

**What an operator should do.** [TBD: stated as conditions, not as a
recommendation for one design.]
