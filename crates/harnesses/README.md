# CAS test harness

`cas-harness` runs storage checks, QEMU fixtures, and IO workloads. Nix packages
it with the pinned guests and backend executables. [Testing](../../docs/testbed.md)
lists the build and run commands.

| Command | Purpose |
|---|---|
| `preflight` | Record host, toolchain, source revision, and worktree state |
| `check-disks` | Read-only check that two declared whole disks are distinct |
| `vm` | Launch a guest and backend, drive IO, and retain evidence |
| `suite` | Run the source-bound checkpoint inventory |
| `verify-suite` | Independently check required records and their hashes |
| `fleet` | Run dated public-image clone/update fixtures |

Use `--help` for options. The disk check does not establish that data on a
device is expendable and never formats it. VM runs need new output directories.
The runner owns child process groups and cleans them up on failure, timeout,
or interruption. Failed runs remain available for diagnosis.

```sh
cargo test --locked -p cas-harness
```

Tests cover evidence validation, process cleanup, invalid inputs, and fixture
behavior. Some storage checks need the documented filesystem/KVM environment;
a native test pass does not imply those fixtures ran.

Retain full fio JSON, source/build identity, environment, exact commands,
seeds, and failure records with each run. Percentiles from separate runs are
not a pooled percentile. Record the tested revision and scope in
[validation](../../docs/validation.md).
