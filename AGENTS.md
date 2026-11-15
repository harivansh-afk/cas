# Development

Keep implementation docs aligned with the current code. `TODO.md` tracks
engineering work; research plans and historical investigations belong in the
private research repository.

Use a task worktree under `.worktrees/`; keep the main checkout on `main`.
Run `just check` for Rust changes and the relevant Nix checks for build
changes. Storage tests need Linux and a checkout filesystem supporting aligned
`O_DIRECT`; live fixtures additionally need KVM and their documented filesystem
features.

For each testing session, update `docs/validation.md` with the date, tested
revision and uncommitted changes, host, commands, outcomes, failures/retries,
and evidence locations. Retain raw output under ignored `results/` or a linked
archive. Separate local checks from CI, deployment, and performance conclusions.
Check off a TODO only when its acceptance condition passes and link the record.
Mark unavailable historical artifacts as unavailable rather than as proof.

Publication uses reviewed files and audited code-only history. Never merge or
mirror unfiltered private research history into this repository.
