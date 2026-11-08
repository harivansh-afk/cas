# Build and test fixtures

## Requirements

Use Linux on aarch64 or x86_64. The Nix flake pins the Rust toolchain, QEMU,
fio, guest configuration, and filesystem tools. Core storage tests create
scratch files inside the checkout and require aligned `O_DIRECT`. VM fixtures
need `/dev/kvm`; XFS tests additionally need real reflink/hole-punch behavior.
Use new output directories and disposable images.

```sh
nix develop
just check
just nix-check
```

`just check` runs rustfmt, Clippy, workspace tests, and `git diff --check`.
Fixture-dependent ignored tests are not exercised by an ordinary workspace
run. Nix evaluation, package builds, and running a guest are separate checks.

## Guest IO and recovery

```sh
nix build .#vm-smoke --out-link result-vm
./result-vm/bin/cas-vm-smoke --output results/raw-run

nix build .#daemon-smoke --out-link result-daemon
./result-daemon/bin/cas-vm-smoke --output results/daemon-run

nix build .#staging-smoke --out-link result-staging
./result-staging/bin/cas-vm-smoke --output results/staging-run
./result-staging/bin/cas-vm-smoke --recovery --output results/recovery-run
./result-staging/bin/cas-vm-smoke --live-recovery --output results/live-run
```

The local/local-async smoke runner passes `--staging-bytes 536870912` (512 MiB)
to `cas-daemon`, including on cold reopen. The two 64 MiB write passes need
space for WAL framing, segment rounding and filesystem allocation metadata;
a 256 MiB cap cannot guarantee completion. Standalone daemon calls still default
to 256 MiB. An explicit cap must be supplied again on restart. Physical allocated
blocks remain charged; this fixture budget does not change `cas-host` policy.

The raw and serial staging paths are controls, not the shared `cas-host`
implementation. The serial live-recovery check does not establish concurrent
shared recovery. Outputs contain build identity, commands, guest logs, and IO
verification results. Keep failures as well as successful runs.

## Source-bound checkpoint suite

```sh
nix build .#checkpoints --out-link result-checkpoints
nix build .#cas --out-link result-cas
nix develop -c result-checkpoints/bin/cas-checkpoints \
  --checkpoint C5 --checkout . --output results/c5-run
result-cas/bin/cas-harness verify-suite --output results/c5-run
```

The shared and pressure fixtures keep their inner guests on TCG. Their immutable
program store is a prebuilt read-only EROFS disk, avoiding a second 9p hop during
boot. It uses SCSI so the vhost-user workload disk remains `/dev/vda`. Results
remain on 9p; guest workloads verify and record the program-store mount.

The suite selects reference checks (C1), packed append (C2), concurrent
recovery (C3), XFS store/collection (C4), and shared filesystem/pressure
fixtures (C5). The harness binds evidence to exact sources and built binaries;
its independent verifier rejects missing or altered required records.
A pass applies only to the tested revision and failure model.

The Nix flake check has a separate 300-second build-validation budget: complete
evaluation took 144 seconds on the isolated eight-vCPU correctness host.
Other command scenarios retain 115 seconds, shared/XFS fixtures 510 seconds,
and pressure 1260 seconds. Guest IO, recovery and admission deadlines remain
independent of this build-validation budget.

## Interactive guests

Use [casctl](casctl.md) for named shared-host labs. A simpler staging-backed
guest is also available:

```sh
nix run .#dev-vm -- --ssh-key ~/.ssh/id_ed25519.pub --output results/dev-vm
```

After IO checks, the runner prints an SSH command. Run `cas-poweroff` inside
the guest to flush and finish; Ctrl-C aborts the session.

## Public-image census fixtures

`nix run .#census-pilot -- results/census-pilot` compares checksum-pinned Ubuntu
root images. `.#census-fleet` boots dated clone/update workloads through
`cas-harness fleet`. The scripts under `experiments/` normalize free ext4
blocks and independently verify exact bytes. These are offline fixtures, not
proof of physical storage savings or current daemon performance. Consult each
command's `--help` before allocating its images.

## Dedicated hosts and evidence

[The host template](../templates/test-host/README.md) documents a destructive
installation on two explicitly selected disks. Building or testing CAS locally
does not require deploying that template.

Record the host environment with:

```sh
cargo run --locked -p cas-harness -- preflight \
  --label development --output results/preflight.json
```

Keep raw results locally under `results/`; record commands, revision, host,
failures, and conclusions in [validation](validation.md). Nested-VM timings,
shared-host measurements, native-device benchmarks, and CI are distinct
results. Do not convert a functional pass into a performance guarantee.
