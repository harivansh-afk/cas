# CAS

A content-addressed block backend for QEMU on Linux. One host process serves
private VM disks backed by a shared chunk store, with local write-ahead logs,
copy-on-write manifests, snapshots, and recovery through vhost-user.

This is experimental software. Use disposable data. Distributed storage,
replication, migration, and hardware power-loss validation are not implemented
or established by the local tests.

## Build and test

```sh
nix develop
just check
```

Linux is required. Storage tests create temporary files in the checkout and
need a filesystem supporting 4 KiB-aligned `O_DIRECT`. The Rust toolchain and
Cargo/Nix dependencies are pinned. `just check` runs formatting, Clippy, Rust
tests, and whitespace checks; `just nix-check` checks the Nix configuration and
builds. See [testing](docs/testbed.md) for KVM and filesystem fixtures.

## Try it

On a Linux host with Nix, KVM, and a systemd user session:

```sh
nix build .#casctl --out-link result-cli
./result-cli/bin/casctl doctor
./result-cli/bin/casctl new demo --count 2
./result-cli/bin/casctl shell demo
./result-cli/bin/casctl ls
```

Keep persistent guest data under `/mnt/cas`; the guest root is disposable.
The lab creates its own backing image rather than formatting a physical disk.
[CLI and lifecycle](docs/casctl.md) covers stopping, restarting, and removing labs.

## Documentation

- [Storage architecture and limits](docs/storage-design.md)
- [On-disk format](docs/storage-format.md)
- [Retained inflight metadata](docs/inflight-format.md)
- [Builds and test fixtures](docs/testbed.md)
- [Current work](TODO.md) and [validation](docs/validation.md)
- [Playbook website](https://cas-playbook.vercel.app/) and [site source](playbook/README.md):
  retained design pages and dated updates; proposals and historical measurements
  are not current release guarantees

## Layout

| Path | Purpose |
|---|---|
| `crates/cas/core/` | Formats, indexes, storage, recovery, caches, and resource accounting |
| `crates/cas/daemon/` | vhost-user frontend, image reactors, shared host, and compactor |
| `crates/cas/cli/` | `casctl` command-line interface |
| `crates/harnesses/` | Regression fixtures, VM orchestration, and IO workloads |
| `crates/vendor/` | Patched upstream crates with their original licenses |
| `nix/`, `templates/` | Reproducible packages, guests, and dedicated-host template |
| `experiments/` | Dated public-image census fixtures and byte-level verification |
| `playbook/` | Static site and PDF source |
| `results/` | Ignored local test output |

## History

The Git history retains the code and build changes from development, including
older crate layouts. Research-only files and extended commit-message bodies
were excluded; retained commits keep their authors and dates but have new
hashes. Historical code is not claimed to pass today's test suite. Use a fresh
clone when moving from the earlier snapshot-only repository; do not merge the
private research history into this one.

## License

Copyright (C) 2026 Harivansh Rathi.

First-party source code is licensed under **GPL-3.0-only**: GNU General Public
License version 3, not "version 3 or any later version". See [LICENSE](LICENSE)
for the terms, including source-distribution obligations and the warranty disclaimer.

Vendored dependencies retain their own licenses and notices under
`crates/vendor/`; this grant does not relicense third-party material. The bundled
Berkeley Mono font is not covered by the GPL grant; its separate redistribution
terms have not been recorded here.
