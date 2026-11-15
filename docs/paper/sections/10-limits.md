# 10 Limits

- **Fleet size.** Latency is measured on [TBD: 2 or 4] hosts. Placement at
  larger fleets is simulated over captured disks.
- **Disk state only.** We do not capture or restore memory. Operators report
  that memory, not disk, bounds sandbox density.
- **Workloads.** RL environments are Python repositories from two benchmarks.
  Other languages and proprietary workloads may share less.
- **Arrival order.** A duplicate is defined against earlier sandboxes, so the
  result depends on the order tasks arrive. [TBD: sensitivity]
- **Network.** Kernel TCP only. RDMA would lower the cost of a remote read.
- **Durability.** The fleet store acknowledges FLUSH locally; we do not
  evaluate replicated durability.
- **Startup in context.** For long tasks, seconds of startup matter little.
  The results bear on short tasks and bursts.
