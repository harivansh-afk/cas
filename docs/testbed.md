# Nix test environment

The flake pins the research tools and provides a KVM guest for testing the raw
storage path. It also exports a NixOS module and a bare-metal host template.
No remote host is provisioned by building or running these development checks.

## Development on Spark or an x86_64 Linux host

```sh
nix develop
just check
nix flake check
```

The shell contains Rust, QEMU, fio, XFS tools, `uv`, `just`, the Nix formatter,
`nixos-rebuild`, and `nixos-anywhere`. `nix build .#cas` builds and tests the Rust
workspace using the same locked Nixpkgs and Cargo dependencies. Linux ARM64 and
x86_64 outputs are provided; the guest must match the host architecture.

Build and run the guest as separate commands:

```sh
nix build .#vm-smoke --out-link result-vm
result-vm/bin/cas-vm-smoke --output results/first-vm-run
```

Once built, `nix run .#vm-smoke -- --output results/another-run` is the equivalent
single command. Each output directory must be new. `/dev/kvm` must be accessible;
the guest refuses software-emulation fallback.

The runner creates a new 128 MiB raw file. QEMU opens it with `cache=none` and
`aio=io_uring`, exposes it as a 4 KiB logical virtio-blk disk, and boots a NixOS
guest with a disposable root filesystem. The guest has no network interface.
It writes 64 MiB through fio, flushes, reads it back with CRC32C verification,
saves its results over a directory share, syncs, and powers off. The forced
power-off avoids teardown of the shared Nix store; persistent guest root state
is not part of this check.

The host checks QEMU's exit, the guest service's exit, fio's error status, and
both byte counts. The default guest timeout is 90 seconds; a failed or timed-out
run exits nonzero and retains its logs. The watchdog terminates the VM process
group. Paths with spaces work; paths with commas or line breaks are rejected
because QEMU parses commas as option separators.

Each run retains `summary.json`, console output, build identity, `flake.lock`,
guest/kernel/device information, the fio job, and fio JSON+ histograms. The raw
image path is recorded in the summary; the image is retained for inspection.
These artifacts stay under ignored `results/` by default.

On a dedicated host, put the scratch image on XFS and the results on the OS
disk using `--disk-dir`:

```sh
result-vm/bin/cas-vm-smoke \
  --output /var/tmp/cas-results/run-001 \
  --disk-dir /srv/cas-testbed
```

The disk directory must already exist. The runner creates a fresh regular file;
it never formats or opens a supplied block device. It records the actual backing
filesystem rather than assuming XFS.

## What this check establishes

This is a **development write/readback check**, including when run on XFS.
It is not R0's paper result and passes no research gate. Spark's ext4 filesystem,
its ARM CPU, and CI's virtual hardware do not substitute for the specified pair
of dedicated test nodes.

The paper baseline still needs fixed host/vCPU affinity, cache and device-state
controls, independent repetitions, the random/sequential workload matrix, and
host device counters. The dated Ubuntu/Debian fleet also remains to be built;
this NixOS guest is a development fixture. CAS integration will replace the
guest's data-disk backend while retaining the guest workload.

## QEMU through cas-daemon

```sh
nix build .#daemon-smoke --out-link result-daemon
result-daemon/bin/cas-vm-smoke --output results/first-daemon-run
```

This runner starts `cas-daemon` on a private Unix socket and boots the same guest
with `vhost-user-blk-pci` and shared memory. The daemon opens the fresh raw image
with `O_DIRECT`, takes an exclusive file lock, and serves one split virtqueue.
It supports reads, writes, FLUSH, and device identification. IO offsets and total
lengths must be 4 KiB aligned; virtio sector addresses remain in 512-byte units.
Requests use owned aligned buffers, with at most 128 requests in flight and a
1 MiB limit per request. The report counts each bounced data request.

The daemon submits file IO through `io_uring`. A FLUSH submits `fdatasync` with
`IO_DRAIN`, waiting for earlier submitted IO and ordering subsequent submissions.
A request succeeds only after its full completion; read data is copied to guest
memory before publishing status. Invalid requests return IOERR when they have a
valid status byte; malformed chains without one stop queue processing. The host
watchdog retains evidence and fails a stalled guest.

The guest runs the original 64 MiB write/readback job, then another 64 MiB job at
queue depth 32 with a flush every 32 writes. The runner checks both fio results,
requires successful daemon exit, and validates daemon byte counts, FLUSH count,
concurrent IO, zero errors, and no outstanding requests at disconnect. Results
include `daemon.json`, `daemon.log`, and `guest/queue.json`. QEMU and daemon
process groups are terminated on failure or timeout. `--disk-dir` also works
with this runner.

The daemon accepts one connection. Staging-store integration, DISCARD,
WRITE_ZEROES, multiple queues, reconnect/replay, and crash recovery remain
pending. These checks do not establish the paper's G1 latency target or G2
recovery gate. [API and protocol references](vhost-user-notes.md) record the
source checks behind the adapter.

## Dedicated NixOS hosts

The common module installs the pinned QEMU/fio tools and `casctl`, sets UTC and
time synchronization, and defaults the CPU governor to performance. The
`bare-metal` module adds key-only SSH and a disko layout: UEFI boot, an ext4 OS
disk, and a separate XFS experiment disk.

Initialize a host flake in a new directory:

```sh
mkdir test-hosts
cd test-hosts
nix flake init --template /absolute/path/to/cas#test-host
```

Follow the generated README to fill in actual disk IDs, SSH keys, architecture,
drivers, and networking. Missing keys and placeholder disk IDs fail configuration
assertions. Installation through `nixos-anywhere` formats the declared disks;
subsequent configuration updates use `nixos-rebuild`. No real node identities or
devices are invented in this repository. The CloudLab pair and its boot/install
path are still pending.

Only the raw-XFS host layout is provided in this iteration. ZFS and CAS experiment
profiles will share this base. Pin and verify a compatible kernel/OpenZFS 2.3
combination when adding R1, rather than treating a newer ZFS as the same arm.

## Source references

- [Pinned NixOS VM module](https://github.com/NixOS/nixpkgs/blob/a5cc6f2c37bf518436dc8d1c288ccd0c43c2f4c4/nixos/modules/virtualisation/qemu-vm.nix): tmpfs root, shared store/directories, runtime launch options, and accelerator checks.
- [QEMU invocation](https://www.qemu.org/docs/master/system/invocation.html): direct IO, io_uring, and disk option parsing.
- [fio documentation](https://fio.readthedocs.io/en/latest/fio_doc.html): write verification and JSON+ histograms.
- [nixos-anywhere](https://github.com/nix-community/nixos-anywhere): initial SSH-based installation with disko.
