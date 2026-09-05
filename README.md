# CAS research implementation

This repository studies content-addressed storage beneath unmodified QEMU.
The research specification is in [docs/spec.md](docs/spec.md), and the rendered
paper is maintained in [playbook/](playbook/README.md).

The first implementation milestone provides a Linux staging-log reference and
contiguous durability tracking. The daemon, census, and paper experiments remain
in development. [Implementation plan](docs/implementation.md) records the build
order, dependencies, acceptance gates, and outstanding design decisions.

Follow [TODO.md](TODO.md) for the short implementation checklist.

## Nix environment and test guest

```sh
nix develop
nix build .#vm-smoke --out-link result-vm
result-vm/bin/cas-vm-smoke --output results/first-vm-run
```

This boots a KVM guest, writes and verifies a new raw disk, and saves its logs and
fio JSON. It is a functional development check. See [the testbed guide](docs/testbed.md)
for the NixOS host template, deployment steps, and remaining paper-baseline work.

## Build and check

Requires Rust 1.89 or newer, Cargo, and a C linker. The storage tests require
Linux and a filesystem supporting 4 KiB-aligned `O_DIRECT`; they create temporary
regular files inside the checkout. There is no buffered-IO fallback.

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
```

To exercise the staging log from the command line, choose a **new** file path:

```sh
mkdir -p results
cargo run --locked -p cas-cli -- staging-check results/staging-check.log
```

The command writes, flushes, overwrites without flushing, reopens, checks the
recovered contents, and emits JSON. It retains the file and refuses to overwrite
an existing path. This check reports no latency or paper-gate result.

Capture the development host's environment with Python through `uv`:

```sh
uv run experiments/preflight.py --label development --output results/preflight.json
```

The inventory command is read-only apart from creating the new output file.
Missing tools are recorded. It does not create filesystems or run benchmarks.

## Layout

```text
crates/cas-core/       durability tracking, aligned IO, staging/replay
crates/cas-cli/        casctl and machine-readable checks
experiments/          environment capture; measurement procedures added by gate
docs/implementation.md  implementation decisions and milestone status
docs/spec.md          research specification
docs/review/          source investigations behind the specification
playbook/             paper website and PDF
results/              local artifacts, ignored by Git
```

`Cargo.lock` is committed. Future storage formats and experimental parameters
will be versioned; the initial staging format is provisional.
