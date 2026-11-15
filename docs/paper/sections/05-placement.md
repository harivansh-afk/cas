# 5 Where Duplicates Sit

A duplicate chunk helps a host only if the host can reach the earlier copy. A
*same-host duplicate* has an earlier copy on the same host. An *other-host
duplicate* has copies only on other hosts. Per-host deduplication shares the
first kind; only a fleet-wide store shares the second.

**Method.** We assign the captured VMs, in arrival order, to H hosts
under a placement policy, and classify each duplicate chunk. Policies:

- *load-based*: the least-loaded host, as operators report doing [@dsec];
- *random*: a uniformly random host;
- *affinity*: a host that already ran a VM of the same repository;
- [TBD: one or two points between load-based and affinity].

For each policy we report the same-host and other-host shares of duplicate
bytes and the load imbalance across hosts. [TBD: imbalance metric]

Placement at 4, 16 and 64 hosts is simulated over captured disks. We check the
simulation against a real run on four hosts. [TBD: only if the four-host
reservation is approved]

**Results.** [TBD: figure, same-host share against H per policy. Pilot, 4 KiB:
affinity keeps 12.1 of 13.2 GB same-host at H = 4 and 16; random keeps 8.8 GB
at H = 4 and 2.3 GB at H = 16.] [pilot]

**Load imbalance.** [TBD. In SWE-bench Verified one repository holds 231 of 500
tasks, so strict affinity concentrates load.]

**Summary.** [TBD: one sentence.]
