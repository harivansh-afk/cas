# Implementation checklist

The storage foundation is merged. The VM backend and research measurements are still ahead.
Tick items as their code, tests, or results land. Details: [implementation plan](docs/implementation.md).

## Done

- [x] Set up the Rust workspace and lock dependencies.
- [x] Build the staging log: writes, flush-to-disk, zero ranges, and recovery.
- [x] Test crashes, torn writes, corruption, ordering, and writer locks.
- [x] Add a command-line recovery check, environment capture, and CI.

## Next: get a VM using our backend

- [ ] Set up a reproducible QEMU/fio environment and test guest.
- [ ] Connect guest reads, writes, and FLUSH to the backend using io_uring.
- [ ] Verify guest data after a backend restart, including requests in flight and multiple queues.
- [ ] Arrange the CloudLab pair and dedicated test disks for paper measurements.
- [ ] Measure the raw-file baseline and compare the backend's guest latency.

## After that

- [ ] Build the dated guest fleet and census; count sharing with fixed-size and content-defined chunks.
- [ ] Add the chunk store, index, manifests, compaction, staging limits, and garbage collection.
- [ ] Pass guest-level recovery tests and compare single-host results with XFS and ZFS.
- [ ] Add two-host ownership, transfers, replication, provisioning, and migration.
- [ ] Measure remote reads, durability costs, cache behavior, and prefetch; add RDMA probes where supported.
- [ ] Produce reproducible experiment runs, analysis, figures, and paper tables.
