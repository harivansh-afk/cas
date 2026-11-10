# Engineering work

Check off work only when its acceptance condition passes, with a record in
[validation](docs/validation.md).

- [x] Serve verified peer chunks over TCP and pass the two-process loopback
  read/error fixture. [Validation](docs/validation.md#2026-11-08--peer-tcp-implementation-and-native-correctness)
- [x] Implement static k=1/2 placement, durable PUT/repair, provisioning, fenced
  migration, sync and coordinated sweep; verify two-process data and crash/retry.
  [Validation](docs/validation.md#2026-11-08--phase-b-static-fleet-transfers-and-crash-recovery)
- [ ] Establish isolated remote-read p99 and GET/PUT contention results on the
  two-host 100 GbE testbed; shared Spark medians are development evidence only.
- [ ] Complete the remaining runtime allocation audit: fetch/request/recovery
  buffers, compaction preparation, diagnostics and allocator arenas; reconcile
  full-daemon RSS. Index and cache component accounting is covered by the
  [chunk-arm validation](docs/validation.md#2026-11-09--fixed-chunk-arms-and-memory-accounting).
- [ ] Bound mixed-IO read tails behind FLUSH barriers and capacity-blocked
  writes without breaking ordering or starving reclamation.
- [ ] Verify mixed readers under actual OOM and ENOSPC with non-expiring
  capacity waits; retain recovery and data-integrity evidence.
- [ ] Rerun the complete C5 inventory on the publication snapshot; earlier
  acceptance belongs to earlier revisions, not this tree.
  [Current rerun](docs/validation.md#2026-11-08--c5-publication-rerun-and-telemetry-tail-regression):
  32/41 passed at `f05b47b`; independent verification failed.
- [ ] Verify sustained workloads on dedicated native storage, separately from
  nested-VM correctness checks.
- [x] License first-party source under GPL-3.0-only while retaining third-party
  terms. [License](LICENSE) · [Scope](README.md#license)
- [ ] Confirm redistribution terms for the bundled font or replace it with a
  font whose redistribution license is recorded.
- [x] Obtain owner approval to publish the retained Playbook content while
  keeping the research archive private. [Publication session](docs/validation.md#2026-09-19--vercel-ci-and-public-repositories)
- [x] Verify automatic Vercel deployment from the GitHub mirror after a main push.
  [Release acceptance](docs/validation.md#2026-09-19--release-acceptance)

Research planning and historical acceptance records are maintained separately
in the private research archive.
