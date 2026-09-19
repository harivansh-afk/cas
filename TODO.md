# Engineering work

Check off work only when its acceptance condition passes, with a record in
[validation](docs/validation.md).

- [ ] Complete the runtime allocation audit, including reader-held buffers,
  compaction preparation, and diagnostic allocations; reconcile charged bytes
  with measured process memory.
- [ ] Bound mixed-IO read tails behind FLUSH barriers and capacity-blocked
  writes without breaking ordering or starving reclamation.
- [ ] Verify mixed readers under actual OOM and ENOSPC with non-expiring
  capacity waits; retain recovery and data-integrity evidence.
- [ ] Rerun the complete C5 inventory on the publication snapshot; earlier
  acceptance belongs to earlier revisions, not this tree.
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
