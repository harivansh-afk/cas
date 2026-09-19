# Local VMs with casctl

Build the CLI on a Linux host with Nix, KVM, and a systemd user session:

```sh
nix build .#casctl --out-link result-cli
./result-cli/bin/casctl doctor
./result-cli/bin/casctl new demo --count 2
./result-cli/bin/casctl ls
./result-cli/bin/casctl shell demo
./result-cli/bin/casctl ssh demo/2 -- lsblk
./result-cli/bin/casctl status demo
./result-cli/bin/casctl stop demo
./result-cli/bin/casctl start demo
./result-cli/bin/casctl stop demo
./result-cli/bin/casctl rm demo
```

`new demo` creates one guest; `--count 2` gives `demo/1` and `demo/2` private
512 MiB data disks in one shared CAS store. `demo` selects the first guest.
The writable ext4 disk is mounted at `/mnt/cas`. The guest root is disposable;
keep anything that must survive a restart under `/mnt/cas`.

Creation returns after SSH is ready. `shell` and its `ssh` alias use ordinary
OpenSSH, with a generated local key and pinned guest host keys. Commands after
`--` are passed as quoted arguments. For remote access, run the CLI through
an existing SSH connection to the host. Guest networking admits the SSH connection;
outbound internet is disabled.

The user service owns the lab after the command exits. `stop` flushes and
unmounts the data disks before powering off; an active workload holding the
mount may prevent that operation. `stop --force` aborts the lab and records no
clean-shutdown acceptance. `start` reopens the same storage using the pinned VM
build. Each boot writes fresh logs and reports. Membership is fixed when a lab
is created; starting or removing one member of a running shared host separately
is not implemented.

`rm` requires a stopped lab. It inventories and removes the 4 GiB backing disk,
VM temporary files and local SSH private key, then archives the small logs and
measurements. The deleted disk cannot be recovered from those reports.

State defaults to `$XDG_STATE_HOME/casctl` or `~/.local/state/casctl`.
`CASCTL_STATE_DIR` chooses another directory. `ls --json`, `new --json` and
`status --json` provide structured output. Human progress goes to stderr.

## Measurements

```sh
casctl bench demo --case read --repeats 5 --seconds 5
casctl bench demo --case write --repeats 5 --seconds 5
casctl bench demo --case flush --repeats 5 --seconds 5
casctl bench demo --case sequential --repeats 5 --seconds 5
```

Each invocation prepares a 32 MiB file, then runs direct fio IO at QD1 with a
fixed random seed. `flush` issues fdatasync after each write. Write completion
latency and sync latency are separate measurements; their p99s cannot be added
to construct a transaction p99. The file is retained for further inspection.
These jobs modify only `/mnt/cas/casctl-bench`.

Reports retain the exact commands, full fio JSON, per-case summaries and
before/after memory/storage samples. Both successful and failed attempts remain.
The host records CAS counters and process PSS/RSS every 500 ms. PSS describes
inner CAS/QEMU processes; adding configured guest RAM again would double-count
pages. The whole lab user service also records cgroup current/peak memory and OOM
events, including the outer VM. That view overlaps inner process memory and
must not be added to it. Host cache and allocator-level attribution remain
separate costs. `status` shows the latest sample, not a proof of peak residency.

CAS telemetry uses bounded rotation: current and previous files, each at most
64 MiB. Process samples retain two files of at most 16 MiB each. Earlier samples
are overwritten in long sessions. Strict checkpoint telemetry retains its
existing terminal limits unless `--telemetry-rotate` is explicitly selected.
In that operational mode, sampling errors stop the sampler and appear in
`host.json` as `telemetry_error`; storage keeps running unless its own shared
failure gate closes. An image socket failure after activation ends that image;
shared failures and incomplete retained recovery still stop all endpoints.

## What runs on the host

A 4 GiB KVM host owns a dedicated XFS filesystem inside a fixed 4 GiB image.
That host runs `cas-host` and one to four 512 MiB TCG guests. This reuses the
existing nested fixture arrangement without mounting or formatting the host's
physical disks. A user-service memory cap of 6 GiB and no swap bounds the lab;
startup requires room for its disk plus 25 GiB free. Runtime checks abort a lab
if host disk headroom falls below 25 GiB. These checks observe shared host space;
they do not reserve it against unrelated writers.

`new NAME --backend raw` uses QEMU's raw-file path; `--backend daemon` uses the
reference raw io_uring daemon. CAS uses four queues; these two controls use one.
All guests have one vCPU and the fio jobs run at QD1. Cache state is not reset
between jobs. Telemetry is enabled in the CAS arm. These are instrumented
development comparisons, not native-device performance or ZFS comparisons.

CLI verbs live in `crates/cas/cli/src/main.rs`. Lifecycle, measurements and
child ownership reuse `crates/harnesses/src/lab/` and `process.rs`. The guest
configuration is `nix/lab/`. Storage semantics remain in `cas-core` and
`cas-daemon`; the CLI never edits manifests or bypasses their owners.
