# 10 Limits

- **Fleet size.** Latency is measured on [TBD: 2 or 4] hosts. Placement at
  larger fleets is simulated over captured disks.
- **Disk state only.** We do not capture or restore memory. Operators report
  that memory, not disk, bounds VM density.
- **Workloads.** RL environments are Python repositories from two benchmarks.
  Other languages and proprietary workloads may share less.
- **Arrival order.** A duplicate is defined against earlier VMs, so the
  result depends on the order tasks arrive. [TBD: sensitivity]
- **Network.** Kernel TCP only. The remote read rate we measure is set by the
  store's read path (§6) and uses under 1% of the 100 Gbit/s link.
- **Implementation.** The store omits readahead, range requests, pre-copy of
  the manifest and transfer of memory state (§6). Its latency and downtime are
  upper bounds. We do not claim what a store with these techniques would
  reach.
- **Durability.** The fleet store acknowledges FLUSH locally; we do not
  evaluate replicated durability.
- **Startup in context.** For long tasks, seconds of startup matter little.
  The results bear on short tasks and bursts.
