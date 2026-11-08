# Validation

## 2026-11-08 — C5 isolated-host continuation

Continuation from `4e3cfb0338810a2330dc14719e01fba36f0557c8` on
`validate/c5-rerun`. Spark retains all earlier evidence. New provider lifecycle,
monitoring and remote-run artifacts are under ignored
`results/c5-isolated-2026-11-08/`. Local command logs continue under
`results/c5-2026-11-08/`. No push or merge is authorized or performed.

### Capacity diagnosis and fixture correction

The two smoke failures were caused by an undersized fixture cap, not incorrect
physical accounting. `st_blocks * 512` includes real allocation metadata and
must remain charged. Two 64 MiB write passes can approach 384 MiB under a
conservative allowance of one header and one commit block per 4 KiB payload,
before explicit flush records and segment rounding. Batching previously hid
this requirement; even the observed batched run needed a fourth 64 MiB segment
and exceeded the 256 MiB cap by 4–8 KiB. Merely subtracting filesystem overhead
or shrinking segments would not make that fixed cap sufficient in all batching
schedules.

`cas-daemon` now accepts an explicit local/local-async `--staging-bytes` limit.
Only the smoke runner selects 512 MiB, enough for this workload plus allocation
headroom. Default daemon/core limits and shared-host policy stay unchanged.
The configured limit is used on creation, rotation, ordinary cold reopen and
retained-frontend cold opening. The regression performs five write/flush epochs
for each execution mode, verifies the default limit still rejects the retained
allocation, then opens with the explicit limit and verifies exact data and
mutation/durability prefixes. The fixture budget is deliberately larger; no
capacity assertion, actual allocation accounting or timeout is weakened.

On the base plus these uncommitted Rust changes, the targeted regression passed
(`capacity-config-green-03.log`). Earlier attempts caught a missing enum
comparison trait and an invalid attempt to use a concurrent-only attachment
operation in the synchronous test; the regression now uses ordinary cold epochs
for both modes. Strict Clippy subsequently flagged default convenience wrappers
that became test-only; they were marked accordingly (`just-check-05.log`).
`nix develop -c just check` then passed formatting, strict Clippy, 512 tests
with 25 existing ignores, and whitespace checks (`just-check-06.log`).

XFS fixture tests now use `--nocapture` and backtraces so panic/assertion
diagnostics survive an outer timeout. Stderr goes to a separate retained
`<module>.stderr.log`: merging uncaptured stderr would split libtest stdout
lines and break the existing strict inventory verifier. The verifier is unchanged. The capacity/readback result also records all 256 individual
1 MiB read durations, from admission attempt through completion (including
buffer allocation); this distinguishes long individual waits from slow aggregate
throughput. On `d40305a` plus this diagnostic change, `just check` passed
512 tests with 25 ignores (`just-check-07.log`). Test selection, serialization
and all deadlines are unchanged.

### Recovered XFS assertion and retry-policy diagnosis

The original `f05b47b` packaged runtime tests were repeated under KVM/XFS on
shared Spark with uncaptured stderr and backtraces. At 21:14–21:20 UTC,
`unique_live_payload_reports_capacity_exhaustion_and_preserves_reads` failed
at the unchanged five-second admission assertion: `full store stranded a read`.
The complete runtime filter subsequently exceeded its 270-second inner bound
and the diagnostic runner its 300-second bound. This is failure evidence, not
a passing fixture. Raw assertion, console, command and CPU/load samples are in
`results/c5-2026-11-08/spark-runtime-nocapture-01/` and
`results/c5-isolated-2026-11-08/spark-runtime-{nocapture-01,resources}.*`.
Spark load was about 90; this reproduction alone cannot isolate contention.

Earlier retained unique-live runs expose a runtime retry defect: the 97.303-second
readback performed 31 collections; the 75.808-second readback performed 25.
Their final sweeps reclaimed no bytes, performed no compaction and reported
capacity exhaustion, yet the worker scheduled another globally quiescing sweep
one second later. Final pauses were 1.958 and 1.472 seconds respectively. Physical
pressure is persistent for unique live data; it is not evidence that another
identical sweep can make progress. These observations explain repeated pauses,
but do not establish that every second of aggregate read time was collection.

The correction suppresses automatic repetition after a completed exhaustion
sweep until retained manifest roots, attachment membership or allocated/promised
capacity changes. Root-set revisions change on publication or final old-pin
release, not ordinary current-root read activity, and are sampled before the
sweep so concurrent release cannot be missed. Pending image work still runs;
manifest advancement re-enables collection. Snapshot roots are immutable and
retained for the host lifetime. Explicit collection always remains available.
The owner refreshes delayed filesystem frees without quiescing readers. A
failed/incomplete collection is not latched as exhaustion. No timeout or
admission deadline changes. Regression coverage checks stable read activity,
old-root release, explicit collection and resumed work after capacity relief.

On `1fa06cd` plus this retry fix, `just-check-08` and the final
`just-check-09` passed. The final check includes formatting, strict Clippy,
513 passing tests, 25 existing ignored fixture tests and whitespace validation.
The XFS-specific regressions await the isolated fixture; a workspace pass does
not exercise them.

### Isolated-host preparation

The provider rejected the requested 16-vCPU Ubuntu VM with `LIMIT_EXCEEDED`:
the current plan permits at most eight vCPUs per VM. A new task-owned
`cas-c5-1108-isolated` VM was created with eight vCPUs, 16 GiB RAM and 64 GiB
disk, Ubuntu 24.04, x86_64 kernel 6.1.102. KVM API version 12 and successful
`KVM_CREATE_VM` were observed as the unprivileged test user. All pre-existing
resources, including active census VMs, are excluded from the cleanup ledger.
A scoped SSH identity grants access only to this task VM; credentials are not
copied into it or included in result archives.

Setup retries are retained: the Ubuntu Nix package requires membership in
`nix-users`; provider exec terminates detached child commands, so persistent
build/monitor jobs use task-owned systemd units. The monitor initially failed
because this kernel omits `/proc/pressure`; PSI is explicitly unavailable.
CPU user/system/idle/iowait/steal, load, memory and free disk are sampled every
second. Build intervals are separate from experiment intervals. Physical-host
load is not exposed by the provider and must not be claimed as measured.

The first XFS run at `e6e9b64` caught an error in the new regression setup:
creating an obsolete root advanced D before creating a WAL namespace, which
correctly rejects `D != 0` (`new staging namespace requires D=0`). The test now
publishes an empty successor root at the same D, which is valid and still tests
last-old-pin release. The runtime retry fix is unchanged. The failed fixture
and uncaptured assertion are retained as `isolate-xfs-fixed-01` (71/72 runtime
tests passed). `just-check-10` caught the corresponding formatting change; after
formatting, `just-check-11` passed all 513 tests with 25 fixture ignores.

### Isolated comparison before the retry fix

At `1fa06cd`, local-sync and async-io passed in 39.42 and 36.01 seconds.
XFS passed in 280.99 seconds, including 72/72 runtime tests (239.20 seconds).
The unique-live 256 MiB readback took 30.444 seconds with 19 collections;
per-1-MiB read median/p99/max were 75.876/730.139/780.596 ms. The final sweep
reclaimed nothing, with a 605.761 ms pause. This confirms repeated no-progress
collections even away from Spark. XFS averaged 92.94% idle CPU, load 0.512
(maximum 1.091), and zero reported steal. No concurrent task builds ran.

The five shared-recovery variants and guest-pressure instead failed reproducibly
on this host:
`recovery deadline expired` at about 61 seconds after phase start, while inner
TCG guests were still booting. Retained complete daemon reports show zero
application reads/writes/flushes and no capacity admission denials; the host
exited before the intended crash boundary. Thus the original failures cannot
all be dismissed as Spark CPU contention. The inner guest's executable store
uses two nested 9p hops, including during NixOS activation. A fixture correction
is being tested: build its immutable program store into a read-only EROFS image
before starting the daemon, attach it through SCSI (preserving CAS at `/dev/vda`),
and assert the actual read-only EROFS mount before running the workload. This
preserves TCG, the CAS data path, memory/CPU geometry, deadlines and all existing
oracles. Failed scenarios averaged 64.76–67.06% idle CPU, with maximum load
1.47–2.60 and zero reported steal. Pressure image reports name the same recovery
deadline; its host report is truncated and is not treated as valid evidence.
Runtime acceptance of this change is pending; it is not yet proof of
which part of boot dominated. `fixture-eval-01` caught formatting differences;
`fixture-eval-02` passed formatting and all-system Nix evaluation.
The complete comparison ended 3/9 passing, 6/9 failing; all raw failures remain.

The comparison artifacts are under the owned VM's `results/isolated/`:
`isolate-*-01/`, matching command receipts/logs, `resources.jsonl`, and
`resource-summary.json`. These will be archived locally before VM deletion.

## 2026-11-08 — C5 publication rerun and telemetry tail regression

Spark/aarch64, host Linux 7.0.14, 128 GB unified memory, accessible KVM;
checkout on ext4, disposable XFS images inside KVM guests. The packaged guest
kernel is 6.18.57; shared fixtures use two TCG inner guests. This session uses
`.worktrees/c5-rerun`, branch `validate/c5-rerun`, `CARGO_BUILD_JOBS=8`, and
`NIX_CONFIG` settings `cores = 8`, `max-jobs = 2`. Other build/test jobs share
the host. These are local development checks, not CI or paper measurements.

### Baseline on current main

Fetched origin/main was `9c60e72676dfb001d14ab9b34583b03ea87f5182`.
The full-history worktree stayed clean throughout the source-bound suite.
`nix develop -c just check` passed 506 tests with 25 fixture-dependent ignores.
`nix build .#checkpoints --out-link result-checkpoints`,
`nix build .#cas --out-link result-cas`, and
`nix develop -c just nix-check` passed before the timed inventory. Builds used
`--cores 8 --max-jobs 2 --print-build-logs`.

```
nix develop -c result-checkpoints/bin/cas-checkpoints \
  --checkpoint C5 --checkout . --output results/c5-2026-11-08/suite-01
result-cas/bin/cas-harness verify-suite --output results/c5-2026-11-08/suite-01
```

All 41 scenarios ran: **31 passed, 10 failed**. The suite exited 1 after
55.9 minutes; independent verification also exited 1, rejecting the required
`nix-check` failure. This is not C5 acceptance.

| Failed scenario(s) | Retained failure |
| --- | --- |
| `nix-check` | Flake evaluation exceeded the unchanged 115-second command deadline; standalone preparation had passed. |
| `xfs-store` | Guest exceeded its 285-second process deadline while the runtime test list was at `unique_live_payload_reports_capacity_exhaustion_and_preserves_reads`. |
| `shared-replacement`, `shared-before-chunks`, `shared-after-chunks`, `shared-after-manifest`, `shared-after-d`, `shared-after-punch`, `shared-after-unlink` | Application readiness expired before the requested replacement/crash boundary; inner-guest logs show incomplete boot. |
| `guest-pressure` | `shared readiness: No data available (os error 61)` before pressure workloads began. |

Host CPU and IO pressure were substantial, with one-minute load exceeding 100.
This is a confounder, not proof that every failure is caused by contention.
The isolated unchanged XFS capacity/read test subsequently passed on real XFS:
256 MiB seeded in 35.858 seconds and read back in 97.303 seconds; writes remained
refused and all readback/allocation assertions passed. Total test time was
137.53 seconds. That establishes progress in the isolated test, not acceptance
of the timed full fixture. The five completed fio smoke records have maximum
read completion latency up to 161.435 ms (concurrent IO, p99 9.503 ms).
No pressure read-tail conclusion is available because those workloads did not run.

### Telemetry fix and regression evidence

A standalone growing-file reader in the same guest reproduced `ENODATA` on
the `/results` 9p mount. An otherwise equivalent reader bounded to each
observed file length passed, checking all 1,564 records / 51,249,152 bytes.
The pressure monitor used the same unbounded tail-read pattern. Its reader now
takes a file-length snapshot, consumes only complete records within that bound,
and revisits partial records on the next poll. IO errors, malformed records,
sample validation and the record-size limit still fail; truncation is rejected.
No timeout, deadline, workload, assertion or daemon policy was relaxed.

On base `9c60e72` plus the uncommitted telemetry-reader change and four tests,
the EOF-error regression failed against the old behavior with errno 61, then
all four regressions passed with the bounded reader. Tests also cover partial
records, concurrent appends, and continued rejection of invalid records and IO
errors. `nix develop -c just check` passed 510 tests with 25 ignores. A full
source-bound inventory on the committed fix is required separately.

All raw output remains under ignored `results/c5-2026-11-08/` in this worktree:
command JSON/log pairs, `suite-01/` (including images, source and build identity),
`inventory-01.json`, `read-latencies-01.json`, resource samples, isolated guest
artifacts, and the failing/passing regression logs. No bulk artifacts were
deleted. Diagnostic setup retries are retained: kernel-command-line injection
did not launch the first XFS probe (that guest was stopped); a path-based Nix
diagnostic build was interrupted and replaced with a Git flake reference; a
Nix override-priority conflict and missing diagnostic scenario file were
corrected. The first regression compile also exposed an import inherited by
the verifier module; its explicit import was restored before the red/green run.
These setup failures are not test passes. No push, merge or deployment occurred.

### Fresh-attachment queue rebase race

Rebuilding the release package at telemetry-fix commit
`8bcee174148821ec131d845187849b2a49754ae0` failed in
`concurrent_retained_fd_replays_without_flush_and_ignores_consumed_available_slots`
with `VhostUserProtocol(PartialMessage)` at queue re-enable. The exact release
test reproduced the failure on repetitions 10 and 23 in separate probes.
This was not dismissed as host contention or retried into acceptance.

A deterministic regression exposed the ordering: a completion wake can activate
a fresh retained attachment while its queue is disabled, between SET_VRING_BASE
and SET_VRING_ENABLE. Queue configuration previously remembered the pending
rebase only for an already-active attachment. If activation occurred between
those messages, re-enable skipped the rebase and failed used-cursor validation,
closing the frontend connection. Fresh queue configuration now also retains
the rebase requirement. Used-cursor validation remains enforced. The socket
test now prints daemon stderr/report evidence before cleanup on panic.

On `8bcee17` plus this uncommitted lifecycle fix, its test and failure diagnostics,
the deterministic test first failed with `memory replacement changed an active
used cursor`, then passed after the fix. `nix develop -c just check` passed
511 tests with 25 ignores. The original release socket test then passed all
30 repetitions, including its crash cuts and ring-wrap cases. No deadline or
assertion was loosened. Evidence: `build-checkpoints-02.log`,
`lifecycle-release-repeat-01/`, `lifecycle-diagnostic-repeat-01/`,
`lifecycle-red-01.log`, `lifecycle-green-01.log`, `just-check-03.log`, and
`lifecycle-fixed-repeat-01/` under the same results root.

The corrected syscall-traced pressure probe of the original binary reached
the existing shared-readiness timeout without reproducing ENODATA. Its syscall
logs are retained in `pressure-traced-02/`; they do not establish the original
error's exact call site. The standalone 9p tail reproduction and regression
above establish the telemetry reader defect independently.

### Release-build fixture failures after the runtime fixes

Package builds on `69c57c43f2f5bbbb9eb008a906fa76c971cc88a2` exposed two
additional failures, retained in `build-checkpoints-03.log` and
`build-checkpoints-04.log`. The continuous-overwrite test missed its assertion
that compaction has completed during an 1800 ms write stream. Twenty isolated
release repetitions subsequently passed (`compaction-release-repeat-01/`).
Its timing failure remains unexplained; host contention is a possible cause,
not an established diagnosis. Its deadline and assertion remain unchanged.

The fresh-attachment epoch test failed cold reopen with `Capacity`. A diagnostic
release reproduction failed immediately and measured three 64 MiB segments,
each with 67,112,960 bytes allocated: 4 KiB above its nominal size. Thus the
fourth segment would exceed the 256 MiB staging limit. The fixture incorrectly
assumed nominal sizes exactly exhaust the budget without filesystem allocation
overhead. The epoch-ordering fixture now uses 8 MiB segments, preserving all
three epochs, the cold reopen, exact data and monotonic mutation assertions.
This changes test geometry, not the production capacity limit or its checks;
no timeout was changed. The existing test is the failing/passing regression.
On `69c57c4` plus this fixture change and record, the targeted release test
passed, then `nix develop -c just check` passed 511 tests with 25 ignores
(`just-check-04.log`).

Evidence is in `capacity-diagnostic-build-01.log`,
`capacity-diagnostic-repeat-02/001.log`, and `capacity-green-01.log` under the
same results root. The first repeat wrapper rejected an ambiguous binary glob
before running a test; the corrected wrapper selected the just-built binary.
The second wrapper recorded the child failure in `results.json` but itself
exited zero; the test log and child exit 101 are the authoritative failure.

### Complete rerun on the committed fixes

The exact clean source was `f05b47b5229b46aba6841b4df21a67860bf2ed42`,
including telemetry fix `8bcee17`, attachment-rebase fix `69c57c4`, and the
epoch fixture geometry correction `f05b47b`. Checkpoint and CAS package builds
passed with release tests (`build-checkpoints-05`, `build-cas-02`).
`nix develop -c just nix-check` passed (`nix-prepare-02`). The source-bound
suite also passed its workspace tests (511 passed, 25 ignored) and its timed
Nix check in 95.545 seconds. No tracked files changed during the suite or
independent verification.

```
nix develop -c result-checkpoints/bin/cas-checkpoints \
  --checkpoint C5 --checkout . --output results/c5-2026-11-08/suite-02
result-cas/bin/cas-harness verify-suite --output results/c5-2026-11-08/suite-02
```

All **41 scenarios executed: 32 passed, nine failed**, in 52.49 minutes.
Independent verification exited 1 with `required scenario local-sync failed`.
This complete receipt does not establish C5 acceptance. The TODO gate remains
unchecked.
The final documentation commit records this result; it is not another tested
runtime revision. Nothing was pushed, merged or deployed.

| Failed scenario(s) | Evidence and remaining diagnosis |
| --- | --- |
| `local-sync`, `async-io` | Both completed their initial 64 MiB write/read verification, then failed queue-depth writes with `staging capacity exhausted`. Three retained 64 MiB segments consume 201,330,688 and 201,334,784 bytes respectively. A fourth nominal segment exceeds the 268,435,456-byte cap by 4 KiB and 8 KiB. `standalone-capacity-02.json` records actual file block counts and daemon statuses. The standalone smoke workload/default geometry needs allocation headroom; the strict cap correctly refused further allocation. This remains unresolved; the unit-test geometry fix does not change these runtime defaults. |
| `xfs-store` | The runtime log marks `unique_live_payload_reports_capacity_exhaustion_and_preserves_reads` **FAILED**. Later tests continued until the outer guest process exceeded 285 seconds at the terminal-close/drain-deadline test. Rust's captured failure details were not printed before the process was killed, so the original assertion and its root cause are unavailable. Advancement past the capacity test is not evidence of a pass. |
| `shared-replacement`, `shared-before-chunks`, `shared-after-chunks`, `shared-after-manifest` | `application readiness expired`. Inner-guest logs show slow NixOS activation/boot; the requested crash boundaries were not reached. |
| `shared-after-unlink` | The requested cut and daemon replacement completed (`compaction-pause.json`, `restart.json`), but the cold verification phase expired while guests were still booting. Exact readback acceptance is absent. |
| `guest-pressure` | `shared readiness: timed out`. The earlier ENODATA did not recur, but no pressure stage was released and no pressure read-latency result exists. |

The after-D and after-punch shared crash-cut fixtures passed completely, including
verification. This run sampled host one-minute load from 16.50 to 88.40 and
minimum free disk space of 244.55 GiB. Contention and nested TCG startup are
important confounders, not proof of the sole cause of every timeout. No timing
bound, workload, verification assertion or production capacity limit was
loosened to obtain a pass.

Across all 82 available completed fio read-job records, the largest recorded
completion latency was 61.065 ms (async smoke p99 5.997 ms); the baseline smoke
maximum was 161.435 ms. No multi-second read completion is established by these
records. This does not bound pressure read stalls: the pressure workload never
started. The baseline isolated XFS test's 97.303-second aggregate readback of
256 MiB remains a throughput concern, not a measured individual-read maximum.
The earlier intermittent continuous-overwrite release-test failure remains
unexplained despite isolated repetitions and subsequent package/suite passes.

Raw evidence remains in `results/c5-2026-11-08/`: `suite-02/`, command JSON/log
pairs, `verify-suite-02.log`, `inventory-02.json`, `read-latencies-02.json`,
`standalone-capacity-02.json`, `resource-samples-02.jsonl`, and
`resource-summary-02.json`. The diagnostic XFS probe below does not replace or
repair the failed full-suite record.

The post-run isolated XFS probe used the exact `f05b47b` package/test binary,
the same kernel/XFS fixture, and the unchanged test with `--exact --ignored
--nocapture --test-threads=1` plus `RUST_BACKTRACE=1`. It passed in 101.77 seconds,
with 256 MiB seeded in 23.843 seconds and read back in 75.808 seconds. All data,
capacity and no-admitted-write assertions passed. Evidence:
`xfs-probe-build-03.{json,log}`, `xfs-isolated-03.{json,log}`, and
`xfs-isolated-03/{command.json,probe.log,unique-live-capacity.json}`. The probe
wrapper's QEMU exit was checked against the actual test summary and result,
not treated alone as success. The failure within the full runtime inventory
was not reproduced; its assertion and root cause remain unresolved. A future
full fixture should retain uncaptured test diagnostics so a later process
timeout cannot hide an earlier assertion. No changes were made to the fixture
runner or test bounds in this session.

## 2026-09-24 — Native KVM runner preparation

On Spark, base `2d56108a2ab38b1985816448acbe6f08d8c29c3e` plus the native
harness, workload script and Nix guest changes in `.worktrees/cloudlab-native`.
`nix develop -c just check` passed: formatting, Clippy, 506 tests and whitespace
checks; 25 live-fixture tests remained ignored. Shell syntax validation and
`nix fmt -- --ci` passed. `nix flake check --no-build --all-systems` evaluated
both supported architectures, including the new native wrapper. Initial Nix
evaluation exposed a missing module argument; explicit guest geometry arguments
fixed it before the passing evaluation. No storage runtime policy changed.

Raw local checks: `results/native-2026-09-24/just-check-01.log` and
`just-check-02.log`. Evaluation is not a guest run or performance acceptance.
Native hardware results and any failures will be recorded separately with the
exact committed revision. [Runner and measurement contract](native-benchmark.md).

## 2026-09-24 — Direct KVM protocol checks and Update 05 draft

On a CloudLab Clemson r6615, the native runner booted NixOS guests directly
under KVM. A live KVM descriptor in each QEMU process confirmed acceleration.
The raw and CAS smoke checks passed at `bd8999d`; passthrough and the two-guest
CAS smoke passed at `806faf9`. These checks used file-backed XFS for development,
not the spare physical NVMe. CRC checks and clean shutdown passed. The physical
device remains unformatted pending owner approval.

A short CAS baseline trial at `806faf9` failed in fio 3.41's verification-state
bookkeeping after the ramp reset the IO issue count. The failed log was kept.
`a1bbdf3` disables unused fio verification-state saving; CRC generation and the
final full-file scan remain enabled. The same short baseline then passed, as
did the two-guest pressure script. No CAS storage behavior or recovery timeout
changed. These protocol results do not accept a performance gate.

The private research repository retains the exact experiment records and
archives. Public inputs, commands and interpretation are in
[native-benchmark.md](native-benchmark.md). The new Update 05 uses the existing
article layout and audited Spark display data. It has no dedicated-NVMe timing
rows yet. The recap separates compaction work counts from the later scheduler
latency result. It also corrects two historical prose claims: the records do
not agree that the control ran first, and they establish continued writer
progress rather than unchanged throughput. Numeric historical data did not change.

Local `pnpm check` and `pnpm build` passed with pnpm 11.5.3. An initial command
from the repository root selected pnpm 12.3.4 and was rejected; running from
`playbook/` selected the pinned version. The standard site build does not itself
generate the separate PDF artifact. Desktop and mobile DOM checks found no
page overflow or duplicate element IDs. Screenshots were captured; this
session's tool runtime could not provide image inspection. The mechanical
design check returned no findings. The draft uses the requested Unslop rules
from `cursor/plugins` revision `12d587dfb20741cafc376c42c696c5f6e2a64487`.

Raw local site checks and screenshots: `results/native-2026-09-24/`. No merge,
production deployment, native-media benchmark, ZFS comparison or full C5 rerun
is claimed here.

## 2026-09-19 — Restore the paper download

Source `87950b617f0bfa0256d82acfbb190ecf6b20e91f` plus this change, tested on
Spark/aarch64 Linux and an Amazon Linux 2023 container matching Vercel's build
OS family. The header PDF icon is unconditional again. Native Vercel GitHub
integration is retained: installation now provisions the PDF toolchain and
publishing builds/checks HTML and the original Pandoc/XeLaTeX paper together.
No deployment token or authentication change is required.

Local checks: frozen pnpm install, Svelte check (zero errors/warnings), and
`VERCEL=1 pnpm build` passed. The paper checker first rejected the absent PDF.
The Amazon Linux installer and subsequent PDF build/check passed: 28 pages,
253,283 bytes, with the original serif/mono typography, figures and chapter
structure. The checker validates the header link order, PDF signature,
parseability, title/final chapter, and writes its SHA-256 sidecar. ShellCheck,
actionlint and whitespace checks passed. Downloaded Pandoc 3.7.0.2 and uv
0.8.22 archives are pinned by version and verified SHA-256.

Failures/retries: an unnecessary attempt to create a project-scoped deployment
token was denied (403); no token or GitHub secret was created. Vercel's documented
dnf installation support made that approach unnecessary. Amazon Linux's existing
curl-minimal conflicted with requesting curl; the installer now uses the existing
curl. An oversized TeX collection install was stopped in the disposable test
container and replaced by explicit required packages. TeX Live 2021 rejected
Pandoc's graphicx `alt` key; a compatibility definition is supplied only when
that key is absent, preserving visible captions. XeTeX auxiliary work now stays
inside its ignored build directory. The pre-PDF HTML crawl's `/spec.pdf` 404 is
expected; the production gate requires a valid PDF before deployment succeeds.

Raw logs, failed checks, generated PDF and visual evidence are retained under
`.worktrees/restore-paper/results/paper-20260919/` and its ignored Playbook build
folders. No runtime changes or storage experiments. Local checks are distinct
from CI and deployment; their remote receipts and live PDF checksum are retained
after the mirrored push.

## 2026-09-19 — Remove the Playbook notice banner

Tested `a16e73a262bca1257814de85b5314eb18aa01f4e` plus the banner/style
removal committed with this record, on Spark (aarch64 NixOS/Linux 6.17.13).
`pnpm --dir playbook install --frozen-lockfile`, `pnpm --dir playbook check`,
`VERCEL=1 pnpm --dir playbook build`, and `git diff --check` passed. Svelte
reported zero errors/warnings. The retained `check.py` verified the banner text
and CSS marker are absent from all 12 generated HTML pages. No failures or
retries; the adapter's zero-config informational warning remains expected.
No runtime or dependency changes. Evidence is retained locally under
`.worktrees/remove-playbook-banner/results/banner-20260919/`. CI and production
verification follow the push; deployment/API and anonymous HTTP receipts are
retained there separately from these local checks.

## 2026-09-19 — Release acceptance

Verified public source `01b98d19cc4118ed3e9a1cb83bb30350da350033` after the
output-directory correction. Only Forgejo was pushed for this commit; GitHub
received the identical SHA through its `main`-only mirror. Vercel's GitHub
integration automatically built production deployment
`dpl_BhTcd4vrRLv8iUPU4zKSFEYiYA9y` from that SHA. It reached `READY`, and
`vercel inspect cas-playbook.vercel.app --json` resolved the public alias to
that deployment. GitHub Playbook run `35459512586` passed all steps, including
Vercel-mode output assertions. No manual deployment or deployment token was used.

On Spark, `uv run --no-project python results/release-20260919/verify_public.py`
passed unauthenticated HTTPS checks for all 12 routes and 36 linked assets;
responses passed the scoped content-marker audit. Raw HTML, hashes, CI metadata
and alias receipts are retained in the release worktree's
`results/release-20260919/`. The public index was also opened in the task's
pinned browser tab. An initial relative screenshot path failed because the
browser session used a different working directory; the absolute-path retry
succeeded.

Both public repositories resolve to the same revision and expose only `main`
on GitHub. Both research repositories remain private and unchanged at
`c80c77a`; anonymous API access to them and to the original research commit
through public `cas` returns 404. The restored history retains 185 audited
code/build commits plus the reviewed baseline and subsequent public changes.
First-party code is GPL-3.0-only; vendored notices and the separate font terms
are not replaced. Local native checks remain the 504-pass/25-ignore result
recorded below; no new storage experiment is claimed.

This acceptance update changes only the tracker and this record. Final remote
SHA/alias receipts for its documentation-only push are retained alongside the
other session artifacts; it does not change the tested site or runtime.

## 2026-09-19 — Public cutover and Vercel output regression

Public `main` was replaced on both forges using explicit leases against
`2291726fc7188744950c86e37351969cd3a15dc9`. Both resolved to
`a9d0663604b4d788d00804c3c93a65f0593c672d`; mirror synchronization then
succeeded without errors. Its tree exactly matches the reviewed/licensed
snapshot (`f9c995b343ac04cb3509a2b50358d20a062cc329`). A fresh GitHub clone
passed fsck and the all-object audit: 186 commits, including the 185 retained
historical code/build commits, no excluded research paths, and no original
research or old snapshot commit objects. Anonymous API reads returned 200 for
`cas` on both forges, 404 for both research repositories, and 404 for the
original research commit through the public GitHub repository. GitHub detects
GPLv3; Cargo/Nix metadata and the README specify GPL-3.0-only.

Vercel is connected to GitHub repository ID `1377398935`, branch `main`, not
the research repository. The first Git-triggered production deployment,
`dpl_FCLCSv1n8Scs91d2uvAdLs7AgZ8v` on `a9d0663`, failed after a successful
install, typecheck and build: adapter-static detected `VERCEL=1` and changed
its default output to `.vercel/output/static`, while hosting expected
`playbook/build`. The previous production site remained live.

Reproduced on Spark, aarch64 NixOS/Linux 6.17.13, source `a9d0663` plus the
uncommitted fix in this record: `VERCEL=1 npx --yes pnpm@11.5.3 --dir playbook
build` completed but `test -f playbook/build/index.html` failed. Explicit
adapter `pages` and `assets` paths now fix the output to `build` in both
environments. GitHub CI now builds with `VERCEL=1` and asserts the index,
numbered chapter and nested update paths.

After the fix, pnpm typecheck passed with zero diagnostics; the Vercel-mode
build produced all 12 HTML pages in `playbook/build`; all three output
assertions and actionlint passed. The adapter's informational warning about
opting out of Vercel zero-config mode is expected. No runtime or dependency
changes were made. These local results do not yet establish the retried
production deployment or new GitHub run.

Evidence: `.worktrees/release-acceptance/results/release-20260919/` contains
the failing reproduction, successful fixed build and lint output. The first
remote deployment log, history audit and pre-rewrite bundle remain under
`.worktrees/vercel-ci/results/{vercel-ci,code-history}-20260919/`. Production
acceptance is recorded after the next mirrored push.

## 2026-09-19 — GPL licensing and code-history restoration

The owner selected GPL-3.0-only for first-party code and explicitly authorized
a guarded replacement of public `main` on both forges with filtered code
history. Complete original history remains in private `cas-research`.

Tested baseline: public `2291726fc7188744950c86e37351969cd3a15dc9` plus the
license/metadata/docs changes in this session. Host: Spark, aarch64 NixOS,
Linux 6.17.13. `reuse download GPL-3.0-only --output LICENSE` from the pinned
Nixpkgs REUSE package supplied the standard license text. All four first-party
Cargo packages, Playbook package metadata and Nix package metadata identify
GPL-3.0-only. Vendored terms are unchanged and the separate font is excluded
from the GPL grant.

- `nix develop -c cargo metadata --no-deps --locked --format-version 1`: all
  four workspace packages report `GPL-3.0-only`.
- `nix develop -c just check`: formatting, Clippy and whitespace passed;
  504 tests passed, 25 fixture-dependent tests ignored, none failed.
- `nix fmt -- --ci` and the Nix package license evaluation passed.
- Pinned pnpm `check` and `build` passed; zero Svelte errors/warnings and
  12 prerendered pages. Runtime source, dependency locks and vendored licenses
  were not changed by licensing.

History preparation used git-filter-repo in an independent disposable Git
repository. The source was the original pre-split main ancestry ending at
`63354b6885179daff980e03f15a15a9c449e794a`, not unmerged research branches.
An explicit allowlist retains Rust and harness sources, vendor notices,
Nix/build inputs, public-image runners and implementation CI, including old
crate layouts. Research documentation/data, website history and unrelated
metadata are excluded. Current approved docs and Playbook are added only as
the public baseline after that filtered history.

The audit examined 419 original commits and verified 185 retained commits:
each retained tree exactly equals its original tree restricted to the
allowlist, and author/committer identities and dates match. All 183 distinct
nonempty selected code states and all 1,328 selected file-content objects are
preserved. The only excluded documents under code/build roots were the three
first-party README files; current approved versions return with the baseline.
Extended commit-message bodies were omitted, co-author trailers retained, and
subjects referring to omitted publication work were narrowed to code changes.
The retained subjects and all selected blobs passed the scoped exclusion-marker
and private-key-header audit; the existing dummy rejection-test string is not
a credential. This is not a claim that every historical commit was rebuilt.

The filter succeeded. A subsequent log preview through `head` returned SIGPIPE
under `pipefail`; a separate clean-status/fsck check and complete audit passed.
No original archive was rewritten. Snapshot-tree equivalence, fresh-clone
object checks, guarded remote replacement and production deployment are checked
separately at cutover; local tests do not establish those outcomes.

Raw commands, filter rules, callback, audit script/results and test logs are
retained locally in `.worktrees/vercel-ci/results/code-history-20260919/`.
The original-to-filtered commit map is retained privately in the isolated
history repository. No physical power-loss or paper experiment ran.

## 2026-09-19 — Vercel CI and public repositories

Source: `94f3c2c1e6949ebdc9f0f09c878de48edca89661` plus this session's
uncommitted deployment configuration and documentation. Host: Spark, aarch64
NixOS/Linux 6.17.13, Node 24.15.0, pnpm 11.5.3. The owner explicitly approved
public access to the clean `cas` repositories and retained Playbook; the
research repository must remain private. No runtime or research data changed.

Local commands:

- `npx --yes pnpm@11.5.3 --dir playbook install --frozen-lockfile`: passed.
- `npx --yes pnpm@11.5.3 --dir playbook check`: passed, zero errors/warnings.
- `npx --yes pnpm@11.5.3 --dir playbook build`: passed, 12 static HTML pages.
- `nix shell --inputs-from . nixpkgs#actionlint -c actionlint .github/workflows/playbook.yml`: recorded in `actionlint.log`.
- Publication audit, JSON parsing and whitespace checks are recorded with the
  session artifacts. Local `.vercel/` project metadata is ignored and contains
  no committed deployment credential.

The existing Vercel project is `cas-playbook`, ID
`prj_ZcPg7KuWPrAVjjvOlZT9DeqYs1uz`, in `rathiharivansh-gmailcoms-projects`.
Preflight found a manual production deployment and no Git connection. The
source-controlled `vercel.json` selects the repository root, static framework,
locked install, typecheck then build, and only `playbook/build` as served output.
Only `main` is enabled for automatic deployments. GitHub CI uses read-only
permissions and no deployment token; Vercel's GitHub app handles deployment.
The automatic build omits optional PDF generation and its download link.

Discovery retries: `vercel` was absent from PATH; pinned CLI 48.10.0
successfully authenticated, then CLI 59.16.0 was used for its authenticated API
command. Both were published over seven days before this session. A historical
project-metadata path and one documentation URL were absent; authenticated
project inspection supplied the settings. No secret files were read or copied.
The browser dashboard used a different account, so project administration used
the verified CLI scope instead.

Raw logs and receipts are retained locally under
`.worktrees/vercel-ci/results/vercel-ci-20260919/`. Local builds do not yet
establish GitHub CI, Vercel deployment, mirror delivery or visibility changes;
those are verified separately after the configuration commit.

## 2026-09-19 — Stable historical research links

Tested `0d32f57ad515278aff07d3bf174d78607c0303c7` plus the link-only change
committed with this record. Host: Spark, aarch64 NixOS, Linux 6.17.13.
Three Playbook pages now pin former `cas-research/main` links to its preserved
`63354b6885179daff980e03f15a15a9c449e794a` snapshot. This keeps source, design
and historical checklist references valid when the private research working
tree no longer contains the implementation or Playbook.

- `pnpm --dir playbook install --frozen-lockfile`: passed; lockfile unchanged.
- `pnpm --dir playbook check`: passed, zero errors and warnings.
- `pnpm --dir playbook build`: passed, 12 prerendered pages.
- Static publication/link audit and `git diff --check`: passed. No remaining
  research-main links in Playbook source. Historical target paths were checked
  in the private archive; anonymous access is not expected.

No failed application checks or retries. A final comment clarification does
not change the compiled links. Runtime, tests, dependencies, build configuration
and displayed research data are unchanged; no Rust suite or experiments were
rerun for this link-only change. Evidence is retained locally under
`.worktrees/research-links/results/research-links-20260919/`.
These are local checks, not CI or deployment acceptance. No visibility or
publication setting changed; remote delivery is checked separately.

## 2026-09-19 — Publication snapshot

**Source:** private development revision
`63354b6885179daff980e03f15a15a9c449e794a` plus the publication cleanup in this
initial commit. Rust implementation, regression tests, Cargo dependencies and
toolchain are unchanged. Documentation, Playbook data imports/links, PDF build
configuration and Nix package metadata changed. Research archives, historical
reviews and automatic website deployment are excluded.

**Host:** Spark, aarch64 Linux 6.17.13, NixOS; checkout on ext4. These are local
development checks, not dedicated-media benchmarks or power-loss tests.

| Command / check | Outcome |
|---|---|
| `nix develop -c just check` | rustfmt, Clippy and whitespace passed; 504 Rust tests passed, 25 fixture-dependent tests ignored, none failed |
| `nix fmt -- --ci` | Passed; 18 Nix files, no formatting changes |
| `nix flake check --no-build --system aarch64-linux` | Passed |
| `nix flake check --no-build --system x86_64-linux` | Passed; evaluation only on this host |
| `nix build .#checks.aarch64-linux.host-config .#checks.aarch64-linux.census-pilot .#checks.aarch64-linux.census-fleet --no-link --print-build-logs` | Passed, including the renamed CAS release package and its native tests |
| `nix flake check --system aarch64-linux` | Passed; check outputs already built |
| `nix build .#vm-smoke --no-link --print-out-paths` | Passed |
| Built `cas-vm-smoke --output results/publication-20260919/raw-smoke` | KVM guest raw-disk IO passed; this is not the shared-host C5 suite |
| `pnpm --dir playbook install --frozen-lockfile` | Passed; lockfile unchanged |
| `pnpm --dir playbook check` | Passed, zero errors or warnings |
| `pnpm --dir playbook build` | Passed, 12 prerendered HTML pages |
| `nix shell --inputs-from . nixpkgs#pandoc nixpkgs#poppler-utils -c uv run --no-project python playbook/scripts/pdf/build.py` | Passed, 27-page PDF; font conversion dependencies pinned |
| Publication audit of tracked files, JSON, Markdown links and built HTML | Passed; six maintained docs, no excluded archive paths or organization-specific markers found |
| PDF text extraction and content-marker audit | Passed |
| Chromium loopback preview | Index, Update 03, Update 04 and its READ operation trace checked; no captured browser errors |

### Failures, retries and limits

- The initial site build requested a not-yet-generated PDF. Ordinary builds now
  omit that link; `VITE_SPEC_PDF=true` enables it for builds that produce the PDF.
  Typecheck and build passed again after the fix.
- The initial publication audit flagged an intentionally invalid private-key
  header in a rejection test and assumed flat update-page output paths. The
  exact dummy string was reviewed and exempted; the audit now checks the actual
  `updates/N/index.html` paths. The corrected audit passed.
- Nix reported a busy evaluation cache during concurrent checks and continued
  successfully. An attempted browser-service query lacked the session-bus
  environment; the already-running browser was reachable directly through CDP.
- Port 43187 was occupied. Vite selected loopback port 43188; the first browser
  attempt timed out against the occupied port. Verification used 43188, in a
  task-owned tab that was closed afterward. Other browser tabs were preserved.
- No shared-host C5 rerun, physical power-loss experiment or native performance
  comparison was performed. Historical Playbook results retain their original
  revisions and are not acceptance of this snapshot.

### Evidence and publication boundary

Raw logs, audit code/results, screenshots, guest output and generated PDF are
retained privately in the research checkout's
`.worktrees/publication-cleanup/results/publication-20260919/` and Playbook
build directories. They are not shipped in this repository. Git backups and
migration receipts are in that checkout's `results/publication-20260919/`.

Both original remote repositories were renamed to private `cas-research`.
Their branch/tag inventories matched the verified pre-cutover mirror backups;
the replacement research mirror targets only the research repository. Fresh
private `cas` repositories were created separately. The publication commit has
no parent; only that snapshot is to be transferred, not the private Git database.
Remote delivery and post-transfer object checks are recorded separately in the
migration receipts. Local checks above do not establish GitHub CI completion.
No Pages site or other public deployment is enabled by this change.

The content audit is a scoped check, not a proof of commercial non-sensitivity
or a complete source-provenance review. Playbook and its displayed historical
measurements are retained by request. First-party licensing and redistribution
rights for the bundled font still require owner review before public release.

## 2026-09-24 — Update 05 architecture explanation

Spark/aarch64 Linux. Base `ecc39e49fcb66ab28a8d16018de04a589273c304`
plus the Update 05 page, its route-specific figure component/data, article title,
and this validation entry in `.worktrees/update5-redesign`. No runtime changes
or new performance experiments. Historical numeric inputs remain unchanged.

The page now explains write/read routing, capacity waits, compaction work,
bounded read discovery, refused-ticket rotation, and the native test setup.
Seven pairs of diagrams carry accessible descriptions. Source links pin the
architecture explanation to the base revision; native runner links retain the
revision in `native.json`. Read p99, maximum latency, and bypass admission are
separate quantities. Native data checks remain separate from unrun dedicated
NVMe measurements.

`pnpm check` passed with zero errors and warnings; `pnpm build` passed.
An initial root-directory invocation selected pnpm 12.3.4 and was rejected;
commands run inside `playbook/` selected the required pnpm 11.5.3. The design
detector returned no findings. A browser inspection at 1440x1000 and 390x844
confirmed paired/stacked diagrams, no document overflow, no duplicate IDs, and
no labels outside the new SVG viewBoxes. Desktop architecture, desktop scheduler,
and mobile architecture screenshots were visually inspected. The first automated
accessibility scan found a keyboard-inaccessible scrolling table; the table
regions now have keyboard focus and accessible names. The final scan reported
zero violations. Svelte's noninteractive-tabindex lint is locally suppressed
with the scrolling-region justification. SVG contrast was marked
incomplete by the automated scan and was inspected visually.

Raw logs, accessibility reports, and screenshots are under ignored
`results/update5-redesign/` in the task worktree. These are local site checks,
not CI, deployment, storage correctness, or new performance acceptance.

## 2026-10-19 — Cargo and flake dependency refresh

Spark/aarch64 Linux, worktree `.worktrees/deps-update`. Tested base
`537b6dbfafccb8d3c665426500e36805ae881ff7` plus uncommitted `Cargo.lock`,
`flake.lock`, and this validation entry. Cargo updates 20 compatible packages;
the flake refreshes nixpkgs, rust-overlay, and disko. The Rust toolchain remains
1.98.1. No runtime source changes or storage-format changes.

Registry review found allocator-api2 0.4.0 as the only incompatible direct
upgrade. It remains on 0.2: current hashbrown 0.17.1 requires allocator-api2
0.2, and CAS passes its BudgetAllocator into hashbrown tables. Changing the
allocator trait version requires a coordinated migration, beyond this refresh.
The patched virtio-queue and vhost-user-backend remain at current upstream
versions with local patches preserved.

Local commands use `CARGO_BUILD_JOBS=3` and Nix `cores=3`, `max-jobs=1`.
`cargo update`, `nix flake update`, `cargo build`, and `cargo test` passed.
Cargo tests passed 506 tests with 25 existing ignored tests.
`just check` passed formatting, strict Clippy, workspace tests, and whitespace.
`nix build -L --cores 3 --max-jobs 1` passed, including release-profile tests.
`just nix-check` passed Nix formatting, all-system evaluation, and all native
flake checks. x86_64-linux outputs were evaluated but not built on this ARM host.

The initial `nix build` client stopped progressing after toolchain preparation.
Only that client was interrupted; the retry with `-L` fetched the remaining
Rust archives and completed successfully. Both logs are retained. No source or
vendor-hash corrections were needed.

Raw command output is retained under ignored
`results/deps-update-2026-10/` in the worktree. These checks do not establish CI,
deployment, KVM fixture acceptance, power-loss safety, or performance results.
