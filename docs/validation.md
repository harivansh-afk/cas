# Validation

## 2026-11-13 — Working-set replay, boot split and first useful IO

Branch `feat/burst-harness`. **Tested revision
`838c4a20e9fc17bd7bc65b476b2853d5f3f2bea1`**, clean on the test checkout;
this record is a later commit on the branch. Harness-only: `experiments/
two-host/{runner,agent,test_runner}.py`, the example configs, `nix/fleet/
fixture.nix` and `docs/two-host-experiments.md` changed; nothing under
`crates/`, so the daemon is still the `ebbe157` build. Nothing pushed or
merged.

Spark handled edits, bundles and evidence; all builds and runs used the
persistent Freestyle VM `cas-build` (`vm-7ecd8e364c624bcb8654fa4dbd92767e`,
x86_64, 8 vCPU, 16 GiB, nested KVM) in the existing clone `/root/cas-burst`,
fetched from a bundle. No VM was created, resized or deleted.

### Commands (cas-build, `/root/cas-burst`; driver `/root/burst/ws-driver.sh`)

```sh
nix build .#two-host --out-link /root/burst/ws/result --cores 6
nix fmt -- --ci
nix develop -c uv run --no-project python -m unittest discover -s experiments/two-host
# Native KVM loopback: both members on one VM, fresh 12 GiB loop-backed XFS
# files at /mnt/fleet-a and /mnt/fleet-b, daemons on vCPUs 0-1 / 2-3, guests 4-7.
/root/burst/ws/result/bin/cas-two-host --config /root/burst/ws/native-ws-001-config.json \
  --output /root/burst/ws/native-ws-001/controller      # cases resume-elsewhere,burst-images
```

Settings: 128 MiB image, 1 MiB data, one-vCPU 512 MiB guests, `resume` =
2 MiB setup, k=1, settled, working set `[dd data 0–256 KiB direct | sha256sum,
dd data 1 MiB–1.25 MiB direct | sha256sum, sha256sum data]`; `burst` = 2
images × (4 MiB base + 1 MiB unique), k=1, no force-remote, owner cache 0,
working set `[dd unique 0–256 KiB, dd unique 512–768 KiB, sha256sum base]`.
Spark ran the same unit tests (`uv run --no-project python -m unittest
discover -s experiments/two-host`).

### Outcomes

- Unit tests: **34 pass** on Spark and on cas-build (9 new: working-set
  parsing/inheritance/overrides and rejection, ring-origin bracketing, first
  remote read with eviction censoring, interval conversion, replay summary,
  agent replay ordering and stdout retention, agent calibration rows, and
  the `checked()` timeout path). `nix build .#two-host` and
  `nix fmt -- --ci`: pass. `just check` was not rerun (Rust unchanged since
  2026-11-12).
- **`native-ws-001`: both cases passed** (`success: true`, schema 3, 106 s).
- `resume-elsewhere` (B's clock, anchor = handoff completion seen on B;
  handoff bracket 250 ms): launch delay 6.7 ms, **boot 18.38 s** (launch →
  ready), SSH banner 18.69 s, first command 20.31 s, marker 20.74 s;
  **first useful IO 7.04 s** after the handoff (bounds 7.00–7.07 s; the
  first remote fetch lands during the ext4 mount, well before `ready`);
  **time to working set 21.22 s**, of which the cold replay was 0.33 s
  (commands 97 / 96 / 136 ms) fetching 313 GETs / 1,289,560 GET bytes on B;
  warm replay 0.27 s (97 / 93 / 75 ms) with 68 GETs, since the two direct
  reads bypass the guest page cache and B retains no fetched chunk — only
  the whole-file checksum became a page-cache hit. Ring calibration: 19 of 32
  direct reads were remote; origin bracket 68 ms wide. Downtime 21.93 s =
  0.57 s stop + 0.50 s handoff + 20.87 s boot/marker/continue (now ending at
  the durable epoch increment); final checksum afterwards 117 ms with 0 GETs
  (page cache). Boot→continue issued 54 GETs; boot→final verify 435.
- `burst-images` N=2 (anchor = first launch; launches 0.6 ms apart): boot
  20.41 / 20.56 s, SSH 20.50 / 20.64 s, marker 22.72 / 22.71 s; first useful
  IO 8.44 s (8.41–8.47 s); 107 GETs by the last marker (4.1 % of used
  blocks); **time to working set 23.20 / 23.20 s** with concurrent cold
  replays of 0.45 s each (115 / 107 / 226 ms and 105 / 109 / 236 ms) fetching
  1,060 GETs / 4,367,200 bytes in aggregate; warm replays 0.29 / 0.28 s with
  128 GETs. 1,541 GETs (58.5 % of used blocks) by the end of verification,
  which now includes the replay. Calibration: 12 of 32 reads remote, bracket
  62 ms.

Nested-KVM, loop-file, single-repetition development checks on one shared
VM: they validate the harness, schema and interval arithmetic, not boot,
latency or placement claims. The TCG fixture was not used (the arms need KVM;
its configuration carries the same working sets for `--cases` runs).

### Failures and retries

- **Attempt 1 (03:21 UTC) failed after 22 s**: `agent.shell(timeout=)`
  collided with `checked()`'s fixed `subprocess.run` timeout, so the first
  guest shell RPC (`write_payload`) raised `TypeError`. The unit tests had
  patched `shell` and missed it. Fixed in `838c4a2` with a test over the real
  subprocess path; the attempt is retained as `native-ws-attempt-1.tgz`
  (not a result). The rerun at 06:21 UTC needed no retry.

### Evidence

Retained under ignored `results/burst-harness/evidence/` with `SHA256SUMS`,
digests matching `/root/burst/archive` on cas-build: `native-ws-001.tgz`
(revision, driver/build/fmt/unittest logs and statuses, config, controller
`results.json`, `config.json`, RPC logs, both hosts' daemon/guest logs,
`qemu.json`, per-command `working-set-*.out` files), `native-ws-001-results.json`,
`native-ws-attempt-1.tgz`. Loop images were unmounted; `/root/burst/ws`
remains on the persistent VM.

## 2026-11-12 — Burst harness: multi-guest reads, burst-images and resume-elsewhere

Branch `feat/burst-harness` from `main` (`ebbe157`, the frozen measured
revision). **Tested revision `7a3c5d267a51648363f94fd0cd93aaccc4e566c6`**, clean
on the test checkout; the fixture-configuration correction, documentation and
this record are later commits on the branch. Harness-only work: no file under
`crates/cas/daemon` or `crates/cas/core` changed, so the `cas` package and its
executable hashes are those of `ebbe157`. Nothing was pushed or merged.

Spark handled edits, Git bundles, provider calls and evidence processing only.
All builds, checks and runs used the persistent Freestyle VM `cas-build`
(`vm-7ecd8e364c624bcb8654fa4dbd92767e`, x86_64, 8 vCPU, 16 GiB, Ubuntu, nested
KVM) in a fresh clone `/root/cas-burst` fetched from a bundle. No VM was created,
resized or deleted.

### Commands (cas-build, `/root/cas-burst`, Nix profile sourced, `ulimit -n 65536`)

```sh
nix build .#two-host-fixture .#two-host --out-link /root/burst/result --cores 6
nix fmt -- --ci
nix develop -c env CARGO_TARGET_DIR=/root/cas/target CARGO_BUILD_JOBS=6 \
  RUST_TEST_THREADS=2 just check
/root/burst/result/bin/cas-two-host-fixture --output /root/burst/fixture-new-001 \
  --cases reads-memory,reads-storage,burst-images,resume-elsewhere \
  --set 'guests_per_host=[2]'                         # TCG attempt, stopped
# Native KVM loopback: both members on one VM, loop-backed XFS files mounted at
# /mnt/fleet-a and /mnt/fleet-b, daemons pinned to vCPUs 0-1 / 2-3, guests 4-7.
/root/burst/result-1/bin/cas-two-host --config /root/burst/native-new-001-config.json \
  --output /root/burst/native-new-001/controller
/root/burst/result-1/bin/cas-two-host \
  --config /root/burst/native-regression-001-config.json \
  --output /root/burst/native-regression-001/controller
```

Settings for both native runs: 128 MiB image, 1 MiB seeded data, 5-second read
windows, one repetition, QD 1/4, 4 MiB/s requested shipping rate, 50 µs inline
polling. `native-new-001`: `guests_per_host=[2]`, one-vCPU 512 MiB guests,
`burst` = 4 images × (4 MiB shared base + 1 MiB unique), k=1, no force-remote,
owner cache 0, no page-cache drop; `resume` = 2 MiB setup, k=1, settled.
`native-regression-001`: defaults (`guests_per_host=[1]`, pinned two-vCPU
1 GiB guests) for `flows-k1,flows-k2,reads-memory,reads-storage,raw-rsync,
independent-stores`. The exact configurations are in the archives.

### Outcomes

- `nix build` of both launchers and `nix fmt -- --ci`: pass. `just check`:
  pass — formatting, strict Clippy, 572 Rust tests passed with 30 fixture
  ignores, **25 Python tests** (12 new: settings normalization and rejection,
  case-plan expansion, overrides, image IDs, guest sizing, fio aggregation,
  burst summary, peer-counter deltas, burst launch ordering, seeded payloads,
  block statistics), whitespace clean. The corrected `fixture.nix` was
  format-checked and built separately afterwards (both exit 0).
- **`native-new-001`: all four cases passed** (`success: true`, 294 s):
  `reads-memory-g2`, `reads-storage-g2`, `burst-images`, `resume-elsewhere`.
- **`native-regression-001`: all six legacy cases passed** (415 s) with the
  historical one-guest schema; their guests ran the pinned QEMU command line
  (no sizing override, `qemu.json` sizing 2 vCPU / 1024 MiB).

Two-guest arms (aggregate over both readers; per-guest p50 in brackets; all
reads CRC32C-verified, quiet labels required zero opposite-source service):

| Arm, cohort | Aggregate IOPS | p50 (ms) | p99 (ms) | Owner counters |
| --- | ---: | ---: | ---: | --- |
| memory, random QD1 | 6,608 | 0.200 [0.198, 0.202] | 2.900 | 33,043 memory, 0 store |
| memory, sequential QD4 | 9,339 | 0.506 | 4.227 | 46,691 memory |
| memory, contention | 5,465 | 0.247 | 3.064 | 27,324 memory, overlap verified |
| storage, random QD1 | 6,666 | 0.276 [0.276, 0.276] | 0.528 | 0 memory, 33,330 store |
| storage, sequential QD4 | 11,212 | 0.659 | 1.794 | 56,052 store |
| storage, contention | 5,058 | 0.334 | 0.979 | 25,292 store, overlap verified |

Aggregate percentiles come from merged json+ bins (bucket lower bounds); the
per-guest fio JSON retains exact values. The contention cohort ran one 2 MiB/s
shipping writer per source guest.

`burst-images` (4 images, k=1, B cold with empty cache): per-image store chunk
deltas after settle were 1,302 then 274, 274, 274 (the shared base deduplicated;
each later image added only its unique region and metadata). Provisioning all
four manifests took 0.803 s and 1,331,714 provision-flow bytes; B's cold restart
recovered in 0.321 s; the four QEMU launches spread over 5.9 ms. Per guest:
launch→ready 30.5–32.0 s, launch→first SSH command 33.2–34.4 s, launch→marker
34.1–35.2 s (nested KVM, one vCPU each, four booting at once), 362 of 1,318 used
blocks read by the marker (0.275 each). B issued 219 GETs by the last marker
(4.2 % of the 5,272 used blocks over all images, 902,280 GET wire bytes
received, peak 3 outstanding) and 2,736 by the end of full verification (51.9 %,
consistent with k=1 placing about half of each image on A; 11,272,320 bytes,
peak 4). A served all GETs from its store; HAS 0; no errors; 149 ring records.
Guest-observed reads exceed GETs because zero/hole blocks and B-owned chunks
are served locally; the attribution caveat is recorded in the case.

`resume-elsewhere` (2 MiB setup, k=1, settled, B cold): downtime 20.772 s =
0.716 s shutdown + 0.295 s handoff (0.239 s cold recovery, 0.042 s daemon
migration) + 19.761 s boot/verify/continue; launch→first command 19.221 s,
launch→marker 19.532 s. Bytes per phase: manifest 131,072, staging tail 20,664
(migration flow 172,388 wire bytes), missing chunks fetched while booting and
verifying 298 GETs / 1,227,760 GET bytes plus 12,922 ship bytes, explicit sync
afterwards 233 bytes. The destination verified the seeded digest and advanced
epoch 0→1. Legacy controls in the regression run: `flows-k1`/`flows-k2`
downtime 19.180 / 19.733 s with provision flow 122,803 / 122,668 bytes;
`raw-rsync` downtime 20.187 s, 134,250,639 bytes per full copy.

These are single-repetition, nested-KVM, loop-file development checks on one
shared VM: they validate the harness and schema, not latency, boot time or
placement claims. `zfs-incremental` was not run (no ZFS on the native host; its
code path is unchanged).

### Failures and retries

- **TCG fixture attempt `fixture-new-001` stopped by the operator.** With the
  initial fixture configuration (six-vCPU outer VM, one-vCPU 512 MiB TCG
  guests) the two concurrently booting sources reached their virtio-blk driver
  about 165 s after launch, but the daemon attaches each image with a fixed
  60-second recovery window (`RECOVERY_TIMEOUT`, attachment to first queue
  activation), so both image services ended with `recovery deadline expired`
  and the guests hung on `set virtio-blk status ... failed`. This is a frozen
  daemon property, not a harness or sizing defect; the retained attempt
  (`fixture-new-001-attempt.tgz`, scratch disks excluded) is not a result. The
  fixture configuration was restored to its four-vCPU, pinned-guest form and
  the multi-guest/burst/resume arms were removed from its TCG default list;
  they remain requestable with `--cases` and need KVM. The second fixture
  invocation (legacy regression) was not started.
- The replacement native loopback runs required no retry. One operator-side
  polling command matched its own shell and exited 143 without effect on the
  run.

### Evidence

Retained under ignored `results/burst-harness/evidence/` with `SHA256SUMS`,
digests verified against the copies left in `/root/burst/archive` on
`cas-build`: `checks.tgz` (build, format, `just check` logs and statuses,
driver scripts, revision), `native-new-001.tgz` and
`native-regression-001.tgz` (controller `results.json`, `config.json`, RPC
logs, both hosts' daemon/guest/fio logs, command lines and `qemu.json`),
`fixture-new-001-attempt.tgz`. The branch bundle `burst-harness.bundle`
(SHA256 `7cf68f26…2a1a`) and `results.json` copies sit beside them. Loop
images were unmounted and deleted after archiving; `/root/burst` remains on
the persistent VM.

## 2026-11-09 — Integration of C5, peer fetch and 16 KiB chunks

Branch `integration/2026-11-09` from `main` (`9c60e72`), merged in order:
`validate/c5-rerun` at `eee52a2` (the 41/41 C5 revision; receipt `e7953c2`
and later commits, including `387757c`, are excluded), `feat/peer-fetch` at
`6f01dca`, and `feat/chunk-16k` at `fdda8ba` (branched from `d49ab61`;
merged rather than rebased). **Tested revision:
`5a1a25e437d40661306978b0b871f2cf94b6128e`**, clean on every test checkout.
This record is a later documentation-only commit. Nothing was pushed or
merged into `main`. These are Freestyle development checks, not CI, paper
or native-hardware results.

### Merge resolution

- Peer fetch × C5: `host.rs`, `worker.rs` and `collection.rs` conflicted.
  Peer fetch moved image construction into `make_image` (also used by fleet
  imports); C5's no-progress collection retry added `Endpoint::swept_roots`,
  `swept_attached` and `Owner::exhausted_space`. `make_image` initializes the
  C5 fields and `Owner` keeps both sides' fields. `Owner::collect` clears the
  exhaustion latch, then refuses local collection on fleet stores, so fleet
  sweeps never latch exhaustion and behave as on `feat/peer-fetch`. Endpoint
  indexes still equal admission slots for imported images.
- 16 KiB chunks: only `storage-design.md` and this file conflicted; both
  sides' text was kept. Its peer/fleet refusal still covers N-member
  configurations: member lists require `--peer-listen`, which it rejects.

### Hosts and commands

No build, test or fixture ran on Spark; Spark handled Git, bundles, provider
calls and evidence reading only. Freestyle x86_64 VMs (Linux 6.1.102,
8 vCPU, 16 GiB, 64 GiB disk, nested KVM):

- `cas-build` (`vm-7ecd8e364c624bcb8654fa4dbd92767e`, retained, reused):
  builds, `just check`, diagnostics, multi-host controller.
- `cas-int-1109-c5` (`vm-18c13982cdb7466ca4f5ebaff7c57433`, task-owned):
  isolated C5 as user `cas` following the accepted procedure (Ubuntu Nix,
  then pinned Nix 2.34.9 from the flake's nixpkgs, `CARGO_BUILD_JOBS=8`,
  Nix `cores=8`, `max-jobs=2`, systemd units); afterwards multi-host member.
- `cas-int-1109-member` (`vm-25335f9da539492cb21febfc7ec9fd89`, task-owned):
  multi-host member.

```sh
nix develop -c just check                              # cas-build and C5 VM
nix build .#checkpoints --out-link result-checkpoints  # C5 VM
nix build .#cas --out-link result-cas
nix develop -c result-checkpoints/bin/cas-checkpoints \
  --checkpoint C5 --checkout . --output results/int/suite-03
result-cas/bin/cas-harness verify-suite --output results/int/suite-03
nix build .#two-host --out-link /root/result-int4-two-host   # cas-build
cas-two-host --config int-n2-002-config.json --output int-n2-002
cas-two-host --config int-n4-001-config.json --output int-n4-001
```

### Outcomes on `5a1a25e`

- `just check` on `cas-build`: **pass** — formatting, strict Clippy, 572 Rust
  tests passed with 30 fixture/subprocess ignores, 13 Python tests, whitespace.
  The C5 VM's own `just check` also passed (572/30).
- **C5: 41/41 passed; independent verifier passed.** Suite 21:06:11–22:01:32
  UTC (3320.8 s, exit 0); verifier 22:01:32–22:02:14 (41.6 s, exit 0). No
  scenario was retried within this run.

| Scenario | Result | Seconds |
|---|---|---:|
| `async-after-active` | PASS | 17.04 |
| `async-after-append-cqe` | PASS | 17.14 |
| `async-after-prepared` | PASS | 17.20 |
| `async-after-status` | PASS | 16.76 |
| `async-after-storage` | PASS | 16.80 |
| `async-after-sync` | PASS | 16.96 |
| `async-after-used` | PASS | 16.78 |
| `async-before-submit` | PASS | 16.78 |
| `async-before-sync` | PASS | 16.94 |
| `async-device-reset` | PASS | 19.04 |
| `async-fresh-recovery` | PASS | 38.51 |
| `async-io` | PASS | 39.39 |
| `async-repeat-after-fence` | PASS | 16.96 |
| `async-repeat-before-fence` | PASS | 17.28 |
| `async-repeat-replay` | PASS | 16.38 |
| `clippy` | PASS | 0.18 |
| `diff-check` | PASS | 0.02 |
| `fresh-recovery` | PASS | 36.06 |
| `guest-pressure` | PASS | 479.34 |
| `live-after-status` | PASS | 17.53 |
| `live-after-storage` | PASS | 18.39 |
| `live-after-used` | PASS | 19.42 |
| `live-before-submit` | PASS | 18.07 |
| `local-fresh-recovery` | PASS | 39.10 |
| `local-sync` | PASS | 42.53 |
| `nix-check` | PASS | 172.63 |
| `nix-format` | PASS | 2.57 |
| `persistence-model` | PASS | 11.49 |
| `raw-daemon` | PASS | 23.47 |
| `raw-qemu` | PASS | 18.39 |
| `rust-format` | PASS | 0.88 |
| `rust-tests` | PASS | 52.86 |
| `shared-after-chunks` | PASS | 239.76 |
| `shared-after-d` | PASS | 251.40 |
| `shared-after-manifest` | PASS | 248.94 |
| `shared-after-punch` | PASS | 256.62 |
| `shared-after-unlink` | PASS | 234.72 |
| `shared-before-chunks` | PASS | 220.73 |
| `shared-replacement` | PASS | 224.95 |
| `staging` | PASS | 25.81 |
| `xfs-store` | PASS | 277.31 |

guest-pressure counted 4 staging stops; 3 of its 617 one-second samples
caught `stopped`.

Multi-host suites (`cas-two-host`, controller `cas-build`, runner
`/nix/store/ghgzr9b6mji6h817418nqba8msamhjjc-cas-two-host`, all hosts at
`5a1a25e`). Settings as in the previous Freestyle runs: 128 MiB image, 1 MiB
data plus 1 MiB drift, 4 MiB/s shipping, 5 s read windows, QD 1/4.

- **N=2 `int-n2-002`: pass, all six cases** (`flows-k1`, `flows-k2`,
  `reads-memory`, `reads-storage`, `raw-rsync`, `independent-stores`),
  22:17:15–22:27:32 UTC, members `10.172.61.2/.3`, daemon vCPUs 0–3, guests
  4–5. k=1/k=2 provisioning, drift sync, sweep, cold reads and migration
  verified data and continued the epoch 0→1; downtime 20.8/20.1 s, handoff
  0.30/0.29 s, migration flow 217,781/217,694 bytes. The first attempt
  `int-n2-001` passed its four CAS cases, then `raw-rsync` failed with
  `Host key verification failed`: the documented A→B SSH prerequisite had not
  been provisioned on the new VMs. It was added and the whole suite rerun.
- **N=4 `int-n4-001`: pass, all five cases** (`flows-k1`, `flows-k2`,
  `reads-memory`, `reads-storage`, `independent-stores`), 22:11:38–22:17:07
  UTC. With a two-VM budget, each VM hosted two members on separate 16 GiB XFS
  loop mounts (members 0/2 on `.2`, 1/3 on `.3`; ports 17483/17485 and
  17493/17495; daemons share vCPUs 0–3; guests on 4–5 and 6–7). This checks
  four-member protocol and placement across two kernels, not four-host
  isolation. Downtime 19.9/22.2 s, handoff 0.42/0.42 s, migration flow
  220,208/229,586 bytes.

Timings are virtual development observations, not performance results. The
ZFS arm (`zfs-incremental`, needing the KVM appliance on two more VMs) was not
run.

### Failures, root causes and fixes

Each fix is a separate commit with a regression test. No timeout, deadline,
admission bound or correctness check was loosened.

1. **Descriptor limit (from `feat/peer-fetch`), `c4df824`.** Each GET client
   preallocates 1,024 slot eventfds plus wake and socket, so one peer host
   exceeds the common 1,024 soft `RLIMIT_NOFILE`. With soft 1024/hard 524288,
   the fleet tests run outside `nix develop` failed with `EMFILE` building a
   single host (measured peak 1,046 descriptors, 1,035 eventfds). `nix develop`
   raises the soft limit itself, which hid this; the chunk-16k session hit it.
   The host now raises the soft limit to the hard limit before creating
   clients and fails startup clearly below `1,026 × clients + 1,024`. All
   daemon library tests then passed run directly at soft 1024; the subprocess
   regression fails with `EMFILE` without the change.
2. **Lock contention after shutdown (test race from `feat/peer-fetch`),
   `839a62a`.** The Nix `cas` package check (first C5 preparation, on
   `c4df824`) failed once: `Store::inspect` returned `WouldBlock` right after
   host shutdown in a fleet test. Store files are flock-held and this binary
   spawns subprocesses; between fork and exec a child shares every open file
   description. Peer fetch had fixed this only in the recovery scan helper.
   The remaining post-shutdown reopen/inspect helpers now retry only lock
   contention for one second, as production opening does. 8+8 repeated
   library runs passed before/after; the deterministic regression fails
   without the retry.
3. **Reflink snapshot test without a store (from `feat/chunk-16k`),
   `de4ef19`.** C5 attempt 1 `xfs-store`: an XFS-only test failed with
   `NotFound` because v3 `Log::create_shared` checks the store's recorded
   geometry and the test created no store. Bisected on XFS: passes at
   `6f01dca`, fails at `fdda8ba`. The test now creates the store first, as
   production initialization does.
4. **XFS runtime selection (from `feat/peer-fetch`), `a5b3c20`.** The fixture
   runs `local::host::tests` with `--include-ignored`; peer fetch added
   fleet/peer modules there, including ignored `daemon_child` subprocess
   entries that fail standalone, and loopback tests that do not use the XFS
   allocation domain. Replayed on `cas-build` XFS: 95 tests in 298 s with two
   failures, versus 76 tests in 260 s without those modules, against the
   unchanged 270 s guest service bound. Both modules are skipped in the
   fixture's list and run; `rust-tests` still runs all of them.
5. **Queue change during shared recovery (pre-existing at `eee52a2`),
   `5aae155`.** Attempt 1 failed `shared-after-manifest` and
   `shared-after-unlink` with `frontend QueueEnable { index: 1, enabled: true
   }: admission before recovery` — the same message as the three `387757c`
   failures. Cause: while retained shared recovery waits, the frontier holds
   restored discovered descriptors and storage is still `Opening`;
   `permits_change` allows queues outside the replay to be configured or
   enabled, and `finish_attachment` handles them, but `begin_change` still
   drained the frontier through admission. It triggers only when a completion
   wake starts replay before QEMU's remaining queue messages, so it is
   timing-dependent; the backend files are unchanged since `eee52a2`. The
   drain is now skipped in that phase (admitted IO there is rejected). The
   XFS regression fails with this exact error on both `eee52a2` and the
   integration without the fix and passes with it. `387757c`'s failures were
   therefore not shown to be caused by read batching.
6. **Sampled pressure assertion (pre-existing fixture), `5a1a25e`.** Attempt 2
   failed only `guest-pressure`: `no recorded staging backpressure`. The check
   required the transient host `stopped` flag in a one-second sample. The
   accepted `eee52a2` run caught 1 stopped sample of 568; attempts 1 and 2
   caught 1 of 637 and 0 of 566, with no runtime change on this path between
   them. Staging usage now carries a monotonic `stops` count; the check
   requires a counted stop, a non-decreasing count and a count for any
   stopped sample. Watermarks, workload and deadlines are unchanged.

### Attempts and retained evidence

- First C5 preparation on `c4df824`: `.#checkpoints` failed in the package
  test phase (failure 2); no scenarios ran.
- Attempt 1 (`839a62a`, `suite-01`): 38/41, verifier failed (failures 3 and 5).
- Attempt 2 (`5aae155`, `suite-02`): 40/41, verifier failed (failure 6).
- Attempt 3 (`5a1a25e`, `suite-03`): 41/41, verifier passed.

Ignored local root `results/integration-1109/`: `evidence/` holds
`int1109-c5vm.tgz` (all three C5 suites, run receipts, `vmstat` samples),
`int1109-member.tgz` and `int1109-builder.tgz` (checks, diagnostics,
bisect/backport/replay logs and multi-host controller output) with
`SHA256SUMS` matching the remote digests; disk images and WAL segments were
excluded. `scripts/` holds launchers and patches; `freestyle/` holds the
pre-existing inventory, creation records, `deleted.jsonl` and the post-cleanup
inventory.

The two task-owned VMs were deleted at 22:30 UTC after export; the readback
lists exactly the ten pre-existing VMs. `cas-build` is retained with its caches
(`/root/cas-int`, `/root/int-target`, `/root/int-results`); this task added
`.2`/`.3` host keys to its `known_hosts` and removed its own XFS scratch file.
No other VM was modified.

### Dynamic image membership (code reading)

A running host cannot add or remove ordinary images: membership is fixed at
start except for fleet provisioning/migration. `host_service::run` requires the
configured endpoints to equal the complete catalog image set (outside fleet
cold mode) and attaches each endpoint once; `Host::from_roots` sizes
admission, fairness, staging quotas and compaction statistics to that image
count; `attach`/`attach_cold` take a one-shot attachment. `Host::snapshot`
adds only immutable snapshot roots at runtime. In fleet cold mode,
`Owner::import_image`/`install` (`local/host/fleet_image.rs`) publish a
provisioned or migrated image into the catalog, build it with `make_image` and
push its attachment and endpoint into one of 64 preallocated slots;
`run_fleet_services` serves it only if its `--image ID=SOCKET` was declared at
start. A migrated-away image is fenced, and a returning migration retires the
old endpoint and removes its catalog entry, but slots are not reclaimed until
restart. There is no general add/remove-image operation.

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

At `849c6d0`, the corrected XFS fixture passed 72/72 runtime tests in 233.35 s
(275.22 s for the logged wrapper). Unique-live readback took 19.819 s; median,
p99 and maximum 1 MiB read times were 77.168, 86.399 and 95.650 ms. The two
initial collections were followed by no collections during readback; releasing
the obsolete root then triggered the asserted retry. CPU averaged 94.10% idle,
maximum load was 1.327, and reported steal remained zero. Compared with the
same-VM baseline, median latency is unchanged while repeated sweep pauses are
removed. `just-check-03` and `nix-check-03` passed before fixture testing.

The first program-image shared test (`isolate-shared-replacement-02`) passed
cold attachment but failed an added fixture assertion before application IO:
NixOS stacks two EROFS mounts at `/nix/store`, and plain `findmnt` returned
`erofs` twice. The assertion now selects the effective mount using
`--target --first-only` and still requires both EROFS and read-only mode.
The fixture also retains the complete boot journal, including a separately
observed Nix registration service failure whose detailed cause was not in the
previous per-workload journal. No deadline or existing oracle changed. The
remaining five focused cases and final suite were gated off after this failure.

At `751d3da`, the corrected mount assertion passed and both applications
completed daemon replacement with the same original QEMU PIDs/start ticks.
The retained boot journal identified the unrelated registration failure:
`register-nix-paths` attempted to create `/nix/store/.links` on the read-only
program image (14.329 s wall time, 64.4 MiB peak memory for the failed service).
The fixed-workload inner guest never invokes the Nix package manager, so its
runtime is now disabled with `nix.enable = false`. This removes an invalid
boot operation rather than changing recovery or application deadlines. Nix
still builds and source-binds the guest and its immutable executable closure.

### Isolated pressure coverage failure

At `838bc5e`, `just-check-05` and `nix-check-05` passed. The five focused
shared replacement/crash variants all passed (`isolate-*-04`), retaining the
original TCG geometry, deadlines and recovery assertions. Guest pressure then
completed all eleven stages in both guests and both fresh-boot content oracles,
but failed the final above-drain coverage assertion. The full inventory was
gated off rather than accepting this partial result.

The controller measured 31,725,231 bytes/s against a calibration of 34,881,328
bytes/s. Its burst interval was 1.057658 s; the two guests recorded 0.656032
and 0.754246 s including deterministic payload generation, direct writes,
pacing and sync. Control publication/observation contributes additional elapsed
time, so guest-local rates cannot replace the conservative controller check.
The VM averaged 60.39% idle CPU with maximum load 3.435 and zero reported
steal; this is not evidence of external CPU contention or a CAS data failure.
Raw failure, all stage records and fresh-boot oracles remain in
`isolate-guest-pressure-04/` and its command receipt.

The next fixture correction prepares the same finite 16 MiB burst payload
before publishing readiness. This removes deterministic hash expansion under
TCG from the offered-load interval. It adds one bounded 16 MiB guest buffer
within the unchanged 512 MiB guest; the reported guest process memory includes
it. Calibration, payload identities, one-MiB direct writes, rate pacing,
terminal sync, controller timing and the above-drain assertion are unchanged.
Preparation duration and bytes are retained in `ready.json`. A regression
compares all prepared bytes with the previous streamed generator and runs the
direct-read identity oracle. Runtime acceptance of this correction is pending.
Spark `just-check-12` stopped at formatting after the helper signature cleanup;
after formatting, `just-check-13` passed formatting, strict Clippy, 514 tests
with 25 fixture ignores and whitespace checks on `838bc5e` plus this change.

At `a2ea96e`, isolated builds, `just-check-06` and `nix-check-06` passed.
The prepared-burst rerun still failed coverage: controller interval 1.268354 s
(26.455 MB/s), calibration 33.852 MB/s, guest intervals 0.910267/0.951228 s.
Guest 0 prepared its unchanged payload in 454.237 ms before readiness. Moving
hash generation alone is therefore insufficient; do not attribute acceptance
to it. The controller's interval also includes command/result propagation and
sampling through the outer guest's `/results` 9p mount.

The next fixture change puts shared control and result files at
`/fixture/shared-results`, separate from `/fixture/store`, and copies them to
the retained `/results/shared` after the harness exits. Inner guests still use
9p, but that export now terminates on the outer guest's local XFS instead of
crossing another 9p mount. CAS storage, guest geometry, offered bytes, measured
controller interval and all assertions/deadlines are unchanged. A Nix regression
checks evidence contents/executable mode and exit-status preservation for both
success and failure. Both focused local Nix builds passed, followed by Nix
format and whitespace checks (`fixture-local-results-*`). Runtime validation
is pending.

One setup probe used `builtins.getFlake` with an absolute path and started
copying ignored target/results files. It was stopped, its two task-owned
processes killed and its exact temporary store directory removed; free space
returned to 36 GiB. This overlapped approximately the first minute of
`isolate-guest-pressure-05` startup, not its burst interval. That startup is not
claimed to be uncontended. The final suite is held until setup is finished.
The probe and cleanup are recorded in `nix-probe-note.json`. Subsequent flake
references must use Git filtering. Ubuntu's base Nix is 2.18.1 versus Spark's
2.34.7; evaluator timing will be checked before releasing the full suite.

### Nix evaluation bound

The task VM installed Nix 2.34.9 from the repository's pinned nixpkgs and
recorded the version inside `nix develop` (`nix-package`, `nix-version`). On
clean `33caf60`, packages, `just-check-07` and complete `nix-check-07` passed.
An explicit `timeout --kill-after=5 115 nix flake check` then failed with exit
124 (`nix-timed-07`), before pressure testing started. The same complete check
with evaluator statistics passed in 143.966 seconds (`nix-isolated-08`). Nix
reported 140.633 CPU-seconds and 7.701 seconds in GC (5.48%); the child resource
record has one major fault and 128 input blocks. This is real evaluation work,
not an IO stall or a failing check retried into acceptance.

**The `nix-check` acceptance timeout is deliberately raised from 115 to 300
seconds.** The generic IO-scenario bound is wrong for this complete package and
NixOS evaluation graph on the isolated supported host. The new bound leaves
roughly twice the measured evaluation time while remaining finite. No Nix
outputs or checks are omitted. Every other scenario, inner-guest deadline,
admission assertion and the pressure above-drain check is unchanged. A regression
checks that the separate Nix budget cannot extend the runtime scenario budgets;
the exception is also emitted in `conditions.json`. This is the only timeout
increase in this continuation.

The first local-control fixture at `33caf60` (`isolate-guest-pressure-06`)
failed immediately and retained the daemon's exact error: `socket/report
directory must lie outside the storage filesystem`. This validates the new
failure-retention path, not C5. The outer fixture now mounts a dedicated
256 MiB tmpfs at `/run/cas-shared-control` and uses its `results` subdirectory;
the storage/report isolation guard is preserved. Earlier complete pressure
evidence occupies about 15 MiB. The mount is recorded in `control-mount.json`;
the same exit handler copies all evidence out. The tmpfs budget belongs to the
outer fixture, not the daemon's payload accounting or either 512 MiB guest.

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

## 2026-11-08 — Peer TCP implementation and native correctness

Spark/aarch64 Linux, kernel `7.0.14`, ext4 checkout on `/dev/nvme0n1p3`,
Rust 1.98.1. Worktree `.worktrees/peer-fetch`, branch `feat/peer-fetch`, based on
`9c60e72676dfb001d14ab9b34583b03ea87f5182`. The tested working tree added the TCP
protocol/service/client, local read and frontend timestamp hooks, config, a
caller-supplied store read scratch API, native tests, and documentation. Runtime
and transport unit tests are committed as `a8b76c3`; the native tests/docs were
uncommitted during the checks below. No unrelated source changes were present.

Commands: `cargo check -p cas-daemon`; focused `cargo test -p cas-daemon --lib
peer -- --nocapture`; `cargo clippy --workspace --all-targets --locked -- -D
warnings`; and `just check`. The final `just check` passed rustfmt, strict
Clippy, 517 tests, and whitespace checks. It reported 26 ignored tests: 25
existing fixture-dependent tests plus the new subprocess entry point, which the
non-ignored parent integration test explicitly invokes. This does not run the
existing ignored KVM/XFS inventory.

The two-process test runs independent daemon Host/reactor runtimes over
loopback, using the native admission/read harness. Both have 64 distinct 4 KiB
chunks; the reader forces remote and does not cache fills. All 700 QD1 reads
(100 warmup plus 600 measured) and four outstanding pipelined reads were checked
byte for byte. The server reported 64 store reads and 640 memory hits. Stopping
the server caused an error without published data. Separate native tests prove
actual absent-index fetch, optional cache reuse, HAS durable-only missing bits,
and local WAL precedence plus FLUSH while compaction is held. Peer-down,
truncated payload, hash mismatch, and MISSING replies fail through the shared
read gate without cache publication or changes to P/E. Transport tests cover
framing/version/length/ID errors, fragmented replies, pipelining (four requests
arrive before any response), deadlines, cancelled slot lifetimes, reconnect, and
unchanged charged transport allocations across hot requests.

Development failures/retries are retained. Initial compilation exposed private
store-helper access and a result type annotation; the final API reuses aligned
caller-owned scratch. Clippy identified fixed-chunk iteration and inline permit
enum size; iteration was corrected, and the two enum expectations explicitly
avoid adding a per-request boxing allocation. An initial seeded manifest tried
to edit D=0 directly; the fixture now uses normal WAL append, FLUSH, and
compaction. The initial QD16 native pipeline exceeded existing image read
credits; the harness uses QD4 and retains admission retry/deadline handling.
A mock writing responses in 37-byte fragments exceeded the two-second request
deadline; the bounded fragmentation regression uses 1,023-byte fragments, and a
separate stalled-peer test checks the deadline. An unnecessary unbounded connect
in the first HAS bounds test was stopped and removed. `strace` was unavailable
because ptrace was denied; no syscall-trace conclusion is claimed.

Raw logs and debug reports are under ignored `results/peer-fetch/`, especially
`just-check-final.log`, `peer-tests-final.log`, `host-tests-*.log`,
`transport-tests*.log`, `pipeline-*.log`, and `debug-loopback-final/*.json`.
Cargo initially resolved relative evidence paths under the crate; those files
were moved into this ignored directory, and documented commands use absolute
paths. Debug latency samples are development diagnostics only. Release-profile
measurements are recorded separately below.

The implemented setup is force-remote on complete local replicas; missing
supposedly local durable dependencies still fail cold recovery. No disk format,
durability prefix, WAL precedence, or recovery rule was relaxed. TCP uses a
bounded nonblocking client worker and blocking service workers; an io_uring
socket driver and busy-polling transport remain unimplemented. Peer failures use
the existing fail-closed host gate, so restoring a peer does not automatically
resume a failed guest attachment. See [peer reads](peer-fetch.md).

These are local development checks. No CI, Nix package validation, QEMU guest
run, CloudLab deployment, cross-host timing, local NVMe comparison, complete C5
rerun, or physical power-loss acceptance was performed. Nothing was pushed or
merged; the main checkout stayed on `main`.

## 2026-11-08 — Release loopback remote-read attribution

Tested exact revision `5253ccf104294cd0cdf6265c446d0855b9cfbc26`, with a clean
working tree and the same Spark/kernel/toolchain/filesystem as above. No code
changes followed this measurement; the final validation commit only records
these results. Command (from the worktree root):

```sh
CAS_PEER_EVIDENCE="$PWD/results/peer-fetch/release-loopback" CARGO_BUILD_JOBS=3 \
  cargo test --release -p cas-daemon --lib \
  local::host::tests::peer::two_daemon_loopback -- --nocapture
```

The release integration passed on its first run. It verified every payload,
retained one TCP GET connection, observed four outstanding GETs in the pipeline
phase, and passed peer-loss error handling. A reported 704 verified chunks and
no reader cache residency; B reported 64 disk/store fills and 640 memory hits.
After B stopped, A reported one error/failed read and still 704 verified chunks.

One run, QD1, 4 KiB, 64 distinct chunks, cache fills disabled on A. Exclude the
first 100 warmup reads; all subsequent 600 measured reads hit B's memory. Times
below are microseconds, computed from the per-request timestamps, with nearest
rank quantiles (`ceil(p*n)`). The harness records receipt before admission and
completion when its blocking completion receive returns. This includes the
real daemon reactor/transport path but excludes QEMU and guest scheduling.

| Phase | Mean (µs) | p50 (µs) | p99 (µs) |
| --- | ---: | ---: | ---: |
| Request receipt → GET send | 55.810 | 25.057 | 796.076 |
| GET send → first reply byte | 97.542 | 56.785 | 1404.885 |
| First byte → received and BLAKE3 verified | 5.995 | 5.760 | 14.656 |
| Verified → harness completion | 84.911 | 24.640 | 2366.212 |
| Receipt → completion | 244.258 | 112.242 | 3247.297 |

Phase means sum to the total mean; marginal phase quantiles do not sum to total
quantiles. First-byte-to-verification includes the remaining TCP receive, hash,
and worker bookkeeping, not pure hashing cost. This is a shared development
host with no CPU/IRQ affinity or idle-state controls, and one short run. The
large tails are measured, not discarded or attributed to an unverified cause.
These numbers establish instrumentation and the exercised path, not the paper's
remote-memory-versus-local-NVMe result. No local NVMe latency arm was run.

Raw evidence: `results/peer-fetch/release-loopback.log`,
`release-loopback/{reader,pipeline,server,failure}.json`, `revision.txt`,
`uncommitted.txt` (empty), `host.txt`, `toolchain.txt`, `filesystem.txt`,
`loopback-link.txt`, `loopback-qdisc.txt`, and `release-binary.sha256`. The retained
`results/peer-fetch/summarize.py` checks 600 successful single-chunk observations,
nonnegative phase durations, and additive means; it emits `latency-summary.json`
and `latency-summary.txt`. Run it with `uv run results/peer-fetch/summarize.py`.
This is an ignored evidence-analysis helper, not a maintained analysis module.
`cargo run --quiet -p cas-daemon --bin cas-host -- --help` also passed and exposed
all four peer flags; output is retained as `cas-host-help.txt`.

Remaining for CloudLab: build the pinned revision on both hosts, bind the
explicit private NIC addresses, configure reachability, prepare matching
compacted replicas, attach the guest/harness, and verify cache hit counters.
Record kernel/NIC/firmware/MTU/IRQ/C-state settings and transport baselines.
Measure controlled local NVMe and peer-memory/peer-NVMe arms with repeated runs,
QD/size sweeps, CPU/IOPS, guest tails, and loss/failure behavior. Long-run phase
capture must account for the bounded 1,024-record ring; periodic telemetry is
not lossless. PUT contention and busy-polling require additional implementation.
No paper performance gate, CI, merge, push, or deployment is claimed.

## 2026-11-08 — Phase A: direct reactor GET and bounded polling

Tested base `7460d42` plus the Phase A worktree changes committed with this
record: shared nonblocking transport engine, reactor-local send/fast completion,
coalesced receive handling, single-receive server GET, bounded optional polling,
and profiling harness. Spark, aarch64, kernel 7.0.14, Rust 1.98.1, ext4/O_DIRECT.
No C5 or unrelated job affinity/priority was changed. All 20 CPUs were close to
fully busy in the initial sample; C5 and unrelated builds/tests shared the host.
Pinning our processes controls placement, **not isolation**.

The default sends immediately from the image reactor on an established socket,
then falls back to the existing worker for slow replies. `--peer-inline-us 20`
adds a bounded polling window and can bypass both transport wakeups. It remains
opt-in: spinning competed poorly with server scheduling in several trials.
Connection setup never holds the reactor mutex. Partial frames, multiple
outstanding GETs, cancelled slots, deadlines, and receiver verification remain.
The server receives a common GET header/hash together; the client receives the
reply header/payload together and retains coalesced short-error/next-frame bytes.
No transport allocation was added per request, and the asynchronous path still
holds the same admission/IO owners. No FLUSH or recovery format changed.

Measurements use the existing native two-process Host harness, 4 KiB QD1,
64 hashes, 100 warmups and 600 timed successful memory reads, reader fills off.
`baseline-test` is the preserved release executable of `7460d42`. Three matched,
interleaved runs of baseline, default direct GET, and 20 µs inline polling pinned
both process trees to CPUs 5–8. Builds were complete before this comparison.
The final counter-only adjustment counts every direct attempt, including the
zero-polling-budget case; measured timing-path code is otherwise identical.
Numbers are microseconds, medians and nearest-rank p99s; ranges span three runs.

| Phase | Baseline p50 range | Direct GET p50 range | Baseline p99 range | Direct GET p99 range |
| --- | ---: | ---: | ---: | ---: |
| Receipt → send | 7.98–8.35 | 5.90–6.14 | 10.48–36.32 | 8.80–14.37 |
| Send → first byte | 9.26–9.81 | 10.58–11.31 | 14.34–980.25 | 183.17–882.66 |
| First byte → verified | 2.38–2.40 | 2.02 | 2.53–6.62 | 2.27–4.43 |
| Verified → completion | 5.20–5.27 | 4.59–5.06 | 8.66–1190.84 | 8.02–17.01 |
| Total | 25.54–26.11 | 23.39–24.30 | 37.68–3027.97 | 732.10–1193.83 |

Default total p50 by run: 24.22, 24.30, 23.39; baseline: 25.54, 26.11,
25.81. The 20 µs polling runs were 17.31/84.16, 61.14/808.56, and
42.10/785.89 (total p50/p99); 491, 373, and 185 of 700 GETs completed
inline. Default direct GET completed 116, 121, and 108 inline. Marginal phase
quantiles do not add to the total. These development runs support a modest
median reduction and a configurable fast completion path. They do not establish
an uncontended p99 improvement or a paper result. The requested p50 <40 µs is
met by the default in all three runs; the sane-p99 target remains unproven here.

The earlier unpinned 112.24 µs median is not a matched code baseline. Three
initial pinned baseline runs already measured 23.86–27.55 µs p50 and
31.87–37.31 µs p99. Separating reader onto CPUs 5–6 and server onto 7–8
produced highly variable 793.78–1059.61 µs baseline and 157.26–1986.81 µs
inline medians. Earlier exploratory runs overlapped a build; retained as such,
not used in the table. These experiments expose sensitivity to scheduling and
interference rather than proving where each tail originated.

Profiling used the Nix-store Linux 7.1 `perf` binary. `perf stat` with task-clock,
context-switches, migrations, cycles and instructions worked. System-wide
`perf sched record` was denied by root-only tracefs; permissions were unchanged.
Per-process `perf record -e cpu-clock:u -F 999 --switch-events` worked. A longer
20,000 verified warmup reads reduced startup/report-formatting dominance. Its
244 user CPU samples include BLAKE3, manifest validation, atomics, memcpy and
mutex contention; this is too few samples for fine-grained cost attribution.
Switch OUT/IN pairs marked `preempt` measured off-CPU p50/p99/max respectively:
GET worker 4/16/3046 µs, server 9/992/8445 µs, reactor 4/47/6274 µs. These
include all lifecycle phases and are not scheduler wake-to-run measurements;
voluntary sleeps are excluded from these figures. They demonstrate preemption
in the measured processes, not causation for every request outlier.

Commands/evidence (ignored `results/peer-latency/`): `compare-final.py` runs the
matched processes and retains `final-{baseline,direct,inline}-{0,1,2}` JSON,
CPU counters, load averages, logs, and executable paths. `compare.py` and
`split-*` retain split-affinity trials. `interference.json`, perf probes and
`profile-direct.{data,log}`, `profile-direct-{report,switches}.txt` retain the
profiling evidence. Profiling command:

```sh
CAS_PEER_PROFILE_READS=20000 CAS_PEER_EVIDENCE="$PWD/results/peer-latency/profile-direct" \
  perf record -e cpu-clock:u -F 999 --switch-events \
  -o results/peer-latency/profile-direct.data -- taskset -c 5,6,7,8 \
  target/release/deps/cas_daemon-30a087ee83203f63 --exact \
  local::host::tests::peer::two_daemon_loopback_verifies_data_measures_phases_and_fails_on_peer_loss \
  --nocapture
```

New regressions cover a coalesced MISSING frame followed by a successful GET and
bounded inline polling that retains a delayed reply for worker completion. The
release peer suite (inline budget 20) passed 15 tests; child entry point ignored
but invoked by the integration. The first `just check` reached tests after fmt
and strict Clippy passed, then an existing attachment test failed with `Capacity`
at `local/tests.rs:605` under default concurrency. Retried with
`RUST_TEST_THREADS=3 CARGO_BUILD_JOBS=3 just check`; final outcome recorded below.
Both full attempts failed identically, and an isolated run of the preserved
baseline executable reproduced the same failure: this was not caused by the
peer changes. The epoch test reopens three retained 64 MiB WAL segments and
allocates a fourth; its 256 MiB reopen limit left no allocation-overhead margin.
The test now allows 320 MiB on reopen. Production limits and recovery accounting
are unchanged. A final plain `just check` follows this fixture correction; logs
retain every attempt.

CloudLab still needs two idle hosts, private 100 GbE NIC binding, matched builds,
separate recorded reactor/server/IRQ CPU placement, cache verification, repeated
blocking/polling comparisons, transport baselines, and the controlled local NVMe
arm. Loopback and native harness timestamps exclude guest/QEMU scheduling and
cannot establish the peer-memory versus ~80 µs local-NVMe headline. No CI,
CloudLab deployment, physical-power-loss test, push or merge occurred.

Final Phase A validation: plain `CARGO_BUILD_JOBS=3 just check` passed fmt,
strict Clippy, all workspace tests (519 passed, 26 ignored), and diff checks.
`just-check-final.log` retains output. The final release peer filter passed
13 tests plus the invoked child entry point (`inline-final.log`); the earlier
15-test result included additional host coverage. The fixture headroom fix
does not change the remote-read benchmark.

## 2026-11-08 — Phase B: static fleet, transfers and crash recovery

Base `ff0bcb0` (the separately committed Phase A) plus the Phase B changes
committed with this entry, on `feat/peer-fetch` in `.worktrees/peer-fetch`.
Spark/aarch64, Linux 7.0.14, Rust 1.98.1, ext4/O_DIRECT. The main checkout was
not changed. No push, merge, CI, deployment or dedicated-filesystem acceptance
was performed. The session continued the existing uncommitted fleet work;
`results/fleet/base-revision.txt`, `worktree.txt`, the final patch and executable
hash pin the evidence. Implementation and operational limits are in
[fleet.md](fleet.md).

Built k=1/2 rendezvous placement over stable two-member membership; a separate
bounded TCP bulk/control service; hash-verified segment PUT with durable ACK and
HAS-based repair; local surplus until coordinated sweep; manifest-only guest
provisioning; (D,E] WRITE/ZERO tail transfer with writer-generation fences;
missing-chunk sync; per-flow application wire counters; and `cas-fleet` commands.
`cas-host` fleet cold mode can serve without guest endpoints and wait for an
explicit incoming image socket. Local-class FLUSH, WAL-first reads and the
existing durable-store/manifest publication order remain. Optional fleet-class
JOURNAL/FLUSH was not added. Standard kernel sockets are used; no new dependency
or io_uring TCP engine was introduced.

The source fence is durable before destination Receiving, and destination Active
follows a verified durable graph and catalog. A cold restart must cover the E
recorded in the writer root, independently of attachment identity. Interrupted
unpublished imports retain their old tickets under unreferenced image names;
return migration retires the old fenced catalog entry before replacement.
Persisted GC epochs prevent restart from silently thawing a partial sweep.
Fenced source roots/WALs, receipts and rejected outputs remain conservative
recovery artifacts; the live chunk-store figures below are not total physical
fleet footprint or the paper's capacity acceptance.

Validation commands (raw logs under ignored `results/fleet/`):

```sh
cargo test -p cas-core --lib transfer -- --nocapture
cargo test -p cas-daemon --lib fleet -- --nocapture
cargo test -p cas-daemon --lib backend:: -- --nocapture
CARGO_BUILD_JOBS=3 just check
CARGO_BUILD_JOBS=3 cargo test --release -p cas-daemon --lib --no-run --message-format=json
uv run results/fleet/measure.py
```

The final full check is `just-check-3.log`: fmt, strict workspace Clippy, all
workspace tests (**534 passed, 27 ignored**), and whitespace checks passed.
The fleet filter passed 14 tests plus its ignored child entry, which the parent
integration tests invoke in independent processes. The frontend regression
filter passed 37 tests. Existing ordering, admission, FLUSH, ZERO/discard,
collection and recovery suites also passed. The native fixture verifies every
read against known bytes, including reads from remote owners and newer WAL data.
It tests both placement modes, provisioning, migration and return, two clones
with common and distinct drift, duplicate sync with no payloads, and local writes
and FLUSH while the receiving peer is down.

Real process-kill coverage includes PUT before insertion and after durable
storage/before ACK; migration after source fence, destination Receiving, partial
transfer, durable destination WAL, catalog publication, and Active publication;
and collection after freeze and after sweep. Restart/retry checks writer
exclusion and the acknowledged tail, then verifies new writes after activation
through another cold restart. An intentionally shortened destination WAL below
the root's E is rejected before repair. GET peer-down, short-read and wrong-hash
IO-error regressions remain in the complete suite. These are daemon SIGKILL and
logical-corruption tests, not physical power-loss or NIC/host-loss tests.

Failures/retries are retained. The inherited WAL transfer test passed before
continuation. Initial subprocess compilation needed corrected private imports
and native attachment types. The first recovery fixture used filesystem-wide
physical accounting on Spark's shared filesystem and hit `observed output
exceeds its disk promise`; native process tests now use the same checked cold
recovery with ungoverned allocation. This does **not** validate the physical
capacity governor under multi-host load. The first GC restart attempt hit a
startup freeze race; a distinct retryable-busy wire status and same-epoch
collector retry fixed it (`gc-crash*.log`). Initial Clippy passes caught style
and type issues. The first full check hung because the new admission fence check
recursively locked a completion gate already held by the frontend; that test
process was terminated, the terminal fence became atomic, and all 37 frontend
regressions passed. The second full check passed the core/daemon tests and found
a duplicate Clap `Config` group; naming the fleet group fixed the CLI parser.
The third full check passed. No timed-out or failed attempt was counted as a
passing performance sample.

Release measurements used the exact executable in `release-build.jsonl`, hashed
in `release-binary.sha256`. `measure.py` ran three repetitions each of the
both-mode flow integration, both-mode drift integration and remote-read
integration, with **both daemon process trees pinned to CPUs 5–8**, builds
finished, and no other job's priority or affinity changed. All nine parent
invocations passed. Raw per-run JSON, logs, `/proc/stat` and load averages are
in `release-{flows,drift,read}-{0,1,2}/`; `release-summary.json` and
`release-measure.log` retain arithmetic and results. CLI help smoke checks
passed (`cas-{fleet,host}-help.txt`). The maintained code's WAL-transfer test also
passed in the full core suite.

**All timings below are development numbers potentially contaminated by shared
Spark load.** The read runs recorded one-minute load averages 129.07, 129.79 and
131.55. Pinning controlled our placement, not isolation from C5 and unrelated
work. No system-wide scheduler permissions or other workloads were changed.
The Phase A matched before/after comparison and perf/off-CPU evidence remain in
the preceding entry; its p99 acceptance is still unproven.

Final-code 4 KiB QD1 peer-memory reads, cache fills off on A, 100 warmups plus
600 verified timed reads per run. Nanosecond records use nearest-rank quantiles;
phase marginal quantiles do not sum to total quantiles:

| Phase | p50 range (µs) | p99 range (µs) |
| --- | ---: | ---: |
| Receipt → send | 5.984–6.016 | 7.824–19.760 |
| Send → first byte | 10.432–10.544 | 21.425–3059.452 |
| First byte → verified | 2.016 | 2.112–5.360 |
| Verified → harness completion | 5.008–5.184 | 7.808–36.497 |
| Total | **23.505–23.664** | **51.488–3530.718** |

Total p50/p99 by run: 23.585/184.785, 23.505/3530.718,
23.664/51.488 µs. Every run also checked the four-request pipeline and peer-loss
IO error. The median target remains met; these samples do not establish a sane
isolated p99 or the peer-memory-versus-local-NVMe headline.

The flow fixture has 64 distinct 4 KiB chunks (262,144 bytes). Before transfer,
compaction/placement complete. All three runs in both modes transferred exactly:

| Flow | Manifest bytes | Tail bytes | Inter-daemon application wire bytes |
| --- | ---: | ---: | ---: |
| Provision | 24,576 | 24 (empty tail envelope) | **28,503** |
| Migrate | 24,576 | 4,152 (one 4 KiB WRITE plus framing) | **32,660** |

Wire values sum the two daemons' **sent** counter deltas for that flow and include
HAS, identity checks, framing, metadata and ACKs. Controller traffic is counted
separately. These byte counts are about a manifest plus tail, rather than the
256 KiB fixture payload. They do not predict arbitrary image manifest packing.

Additive migration phase ranges across the three runs, in milliseconds:

| Phase | k=1 | k=2 |
| --- | ---: | ---: |
| Freeze/drain/FLUSH | 0.026–1.024 | 0.028–0.888 |
| Prepare/hash/check owners | 0.680–0.748 | 0.689–7.241 |
| Source fence + destination Receiving | 24.355–25.240 | 27.686–30.900 |
| File fragments | 0.159–0.187 | 0.137–0.180 |
| Durable install + activation | 83.273–103.419 | 81.768–93.817 |
| Source finalization | 0.031–0.034 | 0.030–0.038 |
| Total | **108.789–129.284** | **113.998–132.003** |

Each individual record's intervals sum exactly to its total; marginal ranges
need not. Provision totals were 112.323–129.764 ms (k=1) and
107.292–117.084 ms (k=2). These administrative paths include metadata/file sync,
connection setup and bounded worker scheduling; no finer attribution or disk
latency comparison was performed. They exclude QEMU/VM transfer and guest resume.

Two separately drifted clones each wrote one common chunk and one distinct
chunk. With k=2, the explicit synchronization sent exactly one distinct 4 KiB
chunk in each direction: 8,192 payload bytes, 13,066 application bytes including
inventory checks. Repeating sync sent no chunk payload and 4,674 control/check
bytes. With k=1, the fixture's two distinct chunks happened to hash to their
producing hosts; neither needed transfer, and both first/repeat sync used 1,092
check bytes. Common content was already present at its owner. Separate k=1
compaction/provision/sweep tests exercise actual remote-owner shipping and reads.
Automatic shipping was paused for this comparison, and the test invokes the
same bounded HAS/PUT primitive; the public sync command is exercised separately
in the flow and peer-outage integrations. These are correctness byte counts,
not sustained chunks/s or 100 GbE throughput results.

After sweep, the retained-root fixture contained 66 unique hashes: k=1 kept
39 on A and 27 on B, with 299,008 encoded store bytes in total; k=2 kept 66 on
each, with 581,632 encoded bytes in total. Reported chunk-index allocations were
6,280 + 1,576 bytes (k=1) and 6,280 + 6,280 (k=2); existing tables can retain
capacity after sweep. Receipt metadata, manifests, WAL/archive files, filesystem
allocation and temporary import files are additional. Native tests intentionally
report physical allocated-before/after as unavailable. The sweep JSON records
pause, copied bytes and removed segments. These tiny fixtures establish placement
and safe surplus removal, not the paper's capacity or half-index hypothesis.

For CloudLab: provision two exclusive storage filesystems and private 100 GbE
interfaces, build this exact revision, configure opposite node IDs and equal k,
and predeclare destination image sockets. Record NIC/firmware/MTU/kernel,
CPU/IRQ/C-state placement and interference; verify cache residency. Run repeated
peer-memory and local-NVMe arms through the real guest, GET/PUT contention,
throughput/chunks-per-second and capacity workloads, and compare transfer bytes
with raw-copy/ZFS/rsync baselines. Add QEMU freeze/device/RAM transfer and resume
orchestration and measure full blackout. Exercise power loss and a physically
dark peer separately from these SIGKILL tests. No CloudLab, KVM, Nix package,
complete C5 rerun or paper performance/capacity gate is claimed.

## 2026-11-08 — two-host experiment controls and harness regressions

Tested `a75a750` plus the peer timing report, zero-capacity cache, GET/PUT regression,
QEMU/SSH runner and harness test changes in this worktree, on shared aarch64 Spark.
Raw evidence is under ignored `results/two-host/`. No push, merge, deployment or
CloudLab result is implied.

- `RUST_TEST_THREADS=3 CARGO_BUILD_JOBS=3 just check` passed in
  `just-check-4.log`: 536 Rust tests passed, 27 ignored; six Python runner tests
  passed, including a real loopback TCP rsync transfer with exact byte readback.
  The new core test verifies that a zero-byte cache retains neither payload nor
  membership, still rejects a hash mismatch, and releases its metadata budget.
- `cargo test -p cas-daemon get_progresses_and_verifies -- --nocapture` passed
  after correcting the test's nonblocking shutdown polling. It stalls an actual
  compaction PUT before insertion/ACK in one process and completes 32 verified
  GETs through its separate listener. No latency threshold is inferred.
- `nix flake check --no-build --all-systems` passed (`nix-eval.log`). Nix package
  tests and fixture builds were also exercised; the final QEMU record follows.
- Early unbounded parallel checks exposed transient `WouldBlock` after IO-owner
  teardown in existing opening/recovery tests, and a repeatable ring-setup race
  in the lifecycle test. The latter now initializes wraparound cursors before
  enabling the queue, preserving the same replay assertions. Twenty consecutive
  focused runs passed (`lifecycle-repeat.log`). A separate harness assertion now
  permits its intentional immediate-exit launcher to already be a zombie, while
  still requiring descendant cleanup. No runtime ordering/recovery rule changed.
- Retained failures/retries: `just-check-initial.log`, `just-check-2.log`,
  `lifecycle-{retry,diagnostic,initial-base,initial-base-2}.log`,
  `nix-package-failure.log`, `nix-build-{2,3}.log`, and
  `python-baseline-test{,-2,-3}.log`. The first baseline attempts incorrectly
  combined a daemon URL with SSH transport options and explicitly requested group
  switching as an unprivileged daemon; the successful TCP test covers both fixes.
  The early fixture attempts found missing VM-specific XFS mounts, the service's
  1,024-descriptor soft limit, unsupported zero-byte chunk cache configuration,
  and a slow TCG guest bootstrap. Those unsuccessful runs are not measurements.

The first guest flow reached provisioning, identical independently written drift,
synchronization, coordinated sweep and verified cold reads in `local-006`.
Migration then failed after QEMU shutdown: the one-shot source attachment had
been consumed. The harness now cold-restarts both daemons after guest shutdown,
includes recovery in handoff downtime, and starts wire deltas after the counter
reset. It does not change attachment, FLUSH, or fencing rules. This failed run's
partial results remain in `local-006/run/results.json`; it is not a successful
migration result. Earlier `local-004` and `local-005` retain TCG boot/first-SSH
login deadline failures and their virtual disks. Readiness now allows 600 seconds
for TCG boot and 120 seconds for its first authenticated SSH command; an owned
persistent SSH master avoids repeated guest login setup.

Follow-up to `0ed54d0`: PUT metrics now count chunks only after successful durable
ACK, and contention requires PUT acknowledgments and verified GET progress in
the same interval. Inventory HAS bytes cannot pass this check. The drift test
checks ACK counts against actual transferred chunks; its focused run passed in
`put-ack-test.log`. Seven Python tests pass (`python-final.log`), including the
new inventory-only rejection. Observation follows fio until process exit so
slow guest command startup cannot move all IO outside the sampling window.
Local fixture cohorts now last five seconds. `nix fmt -- --ci` passed in
`nix-format-final.log`. Final suite and QEMU rerun results follow below.

`RUST_TEST_THREADS=3 CARGO_BUILD_JOBS=3 just check` passed again in
`just-check-5.log` with 536 Rust tests passed, 27 ignored and seven Python tests
passed. This covers the ACK counter change and all existing fleet crash/fencing
regressions. The harness additionally records controller elapsed time for each
explicit fleet command, separately from drift generation/background repair.

Builds of both `.#two-host-fixture` and `.#two-host`, and all-systems flake
evaluation, passed (`nix-build-verified.log`, `nix-eval-last.log`). The committed
`eba8f53` fixture attempt `local-007` exposed a readiness race under load ~60:
the disk/host-key markers preceded static interface configuration and the SSH
master timed out during banner exchange. Its logs and failed disks are retained.
Readiness now probes the actual forwarded SSH banner before the owned master
starts; pinned-key authentication remains mandatory. A loopback unit test
rejects non-SSH banners and absent listeners. The authenticated handshake allows
120 seconds under TCG scheduling interference.

`local-008` failed during guest startup: a one-second banner probe repeatedly
closed SSH sessions before their TCG-scheduled banners arrived. The probe now
allows 30 seconds per attempt within the existing readiness deadline, avoiding
that retry pressure. No storage/performance result is claimed from this attempt.


`66dec34` passed `just check` in `just-check-final.log` (536 Rust tests,
27 ignored, eight Python tests). Its `local-009` guest run verified provisioning,
drift and cold remote reads, then reached a durable destination activation but
reported migration failure. Focused replay from copies of retained XFS disks
(`diagnose-001` through `diagnose-003`) isolated the cause: an administrative
frontend wake before feature negotiation was treated as guest IO, failing the
source's dormant vhost service during handoff finalization. Root records showed
A fenced at generation 2 and B active at that same generation; a cold retry
succeeded. There was no evidence of two writers or data corruption.

The frontend now ignores only completion/administrative wakes while feature or
memory setup is incomplete, while retaining terminal gate checks. Real queue
kicks still fail before setup. `host-wakeup-test.log` covers each partial setup
and asserts no descriptor consumption. A two-process test with a real unconnected
vhost service failed before the fix (`cold-service-before.log`, 30.69 s) and
passed after it (`cold-service-after.log`, 1.20 s), verifying migrated bytes.
Bulk operation failures are now logged in production, not only test builds.
The harness also restarts A without frontend listeners and B with only the
migrating image, so stopped clones do not leave unused timed listeners.

The diagnostic Nix expression initially used an unfiltered path flake reference;
that evaluation was canceled, then rebuilt successfully using `git+file://`.
Diagnostic scripts/expressions, disk copies and logs remain under ignored
`results/two-host/diagnose*`. They are diagnostic artifacts, not a runner mode.


`d49ab61` passed `just check` (`just-check-wakeup.log`: 538 Rust passed,
27 ignored, eight Python passed). The fixed binary also passed first-attempt
handoff from the retained pre-fence disks with a real dormant frontend
(`diagnose-004`), where the prior binary logged feature-negotiation failure.
Both release packages built (`nix-build-handoff.log`).

`local-010` completed the entire k=1 guest flow with SHA verification and durable
epoch continuation on B. Its migration downtime was 201.399 s: stop 2.348 s,
recovery/handoff 0.626 s (cold recovery 0.336 s), boot/verification 198.425 s.
The daemon transfer took 263.106 ms; its manifest was 294,912 bytes and tail 24
bytes. Migration-flow traffic was 316,205 bytes, plus 25,623 background-shipping
bytes during that window. Provisioning used 168,030 flow bytes plus 4,381 shipping
bytes. These are application bytes on virtual disks with nested TCG, not paper
performance. The raw run then failed in k=2 clone SSH readiness: the disk mounted,
but device-triggered static network setup did not make eth0 reachable within the
180-second probe deadline. The partial k=1 result is valid; the whole run is not
certified. The isolated guest now loads virtio_net early and explicitly configures
its one restricted user-network interface before sshd starts. Host networking is
unchanged. This removes the device-unit ordering dependency rather than extending
the readiness timeout again.


`f2c518f` built both runner packages, passed Nix formatting and all-systems
flake evaluation (`nix-build-network.log`, `nix-network-fmt.log`,
`nix-eval-network.log`). `local-011` confirmed the new network unit completed
before SSH startup, but exceeded the ten-minute TCG boot deadline under Spark
load above 100 and CPU pressure around 70–80%. It did not reach a workload.
`physical-load.jsonl` and `cpu-placement.json` retain physical-host evidence;
the existing performance-core pin was retained. TCG boot now allows twenty
minutes and the local suite four hours, with a boot RPC envelope covering its
readiness phases. Native KVM and storage/protocol deadlines are unchanged.
The fixture now records physical host pressure automatically every five seconds.

## 2026-11-09 — interrupted Spark validation; move execution to Freestyle

Worktree `feat/peer-fetch`, revision `f650a2b` plus the fixture sampler cleanup
and capacity-method clarification. `CARGO_BUILD_JOBS=2 just check` completed on
Spark before the owner's interruption: 538 Rust tests passed, 27 ignored, and
eight Python tests passed (`results/two-host/just-check-pressure.log`). Ruff
passed (`ruff-final.log`). All-systems Nix evaluation was interrupted
(`nix-eval-pressure.log`). The owner terminated `local-012`; its launcher exited
143 and the guest flow did not complete. Retained scratch disks, console and
physical pressure records are incomplete evidence, not a passing fixture.

All earlier Spark latency and guest downtime numbers are contaminated by shared
host interference and are development diagnostics only. From this point, Spark
is used only for editing and orchestration: no builds, tests or fixtures. New
validation runs on x86_64 Freestyle VMs. The provider rejected the requested
32-vCPU VM with `LIMIT_EXCEEDED` (account maximum 8 vCPUs), so the persistent
`cas-build` host uses 8 vCPUs / 16 GiB. Inventory and created-resource IDs are
retained in ignored `results/freestyle/`; existing VMs are not modified.

## 2026-11-09 — N-member protocol on Freestyle x86_64

Tested `2574a8e` plus the N-member runtime/test changes, on the newly created
persistent `cas-build` VM (8 vCPUs, 16 GiB, 64 GiB disk, AMD-V/KVM, Linux 6.1.102).
The provider also rejected a 128 GiB disk resize; the persistent cache remains
64 GiB. No existing VM was modified. The initial unchanged revision passed
remote `nix develop -c env CARGO_BUILD_JOBS=6 just check` before these edits.

Final runtime validation: `nix develop -c bash -c 'cargo fmt --all &&
CARGO_TARGET_DIR=/root/cas/target CARGO_BUILD_JOBS=6 just check'` passed: 541 Rust
passed, 27 ignored, eight Python tests passed, strict Clippy and formatting clean.
Evidence: `results/freestyle/n-members-full-5.log`; earlier attempts and diagnostic
backtraces remain under `/root/cas-results` on `cas-build`. This verifies native
four-process loopback, not yet four-VM or QEMU/ZFS execution. The expanded runner
and incremental ZFS arm are separate work in progress at this checkpoint.

New N=4 tests cover k=1/k=2 exact owner counts after surplus collection, remote
reads, provisioning, writes while a peer is down and HAS/PUT repair after restart,
duplicate sync, migration through three writer locations, retained tail bytes,
and kill/retry after the source fence, after witness publication and after
receiver activation. A third-member witness is persisted separately from the
retired local image root: remote writes must never raise that old local WAL's
recovery floor. Restart of the old source before returning migration verifies
this distinction. Initial identity claims require every member's acknowledgment.

Failures resolved during this session:
- A repeated read after peer restart failed with `UnexpectedEof`; the client now
  observes idle socket EOF rather than leaving a stale connection for the next
  guest request. The four-member flow reproduces and covers the fix.
- Port-0 reservations could be stolen by outgoing connections before subprocess
  listeners bound. Fleet tests now allocate distinct ports below the default
  Linux ephemeral range. A peer-loss assertion also incorrectly expected remote
  partitioned reads while their owner was down; data verification now follows
  repair, while acknowledged writes are still issued during the outage.
- Concurrent process tests exposed transient `WouldBlock` in existing recovery
  inspection tests. Their helper now retries only lock contention for one second,
  matching production opening's bounded contention handling. Persistent contention
  and other errors still fail; no runtime recovery or durability rule changed.
- Initial compilation caught an ambiguous integer and an immutable query buffer;
  Clippy caught a redundant cast. All were fixed before the passing run.

No latency conclusion is supported by this correctness run. Cross-VM QEMU flows,
common-base ZFS stream measurements and the complete runner remain to validate.

## 2026-11-09 — N-host runner and real incremental ZFS baseline

Tested `37d92f1` plus the runner/appliance changes on Freestyle `cas-build`.
`nix develop -c` Ruff formatting/check and Python unittest discovery passed
(11 tests), including real TCP stream/receiver acknowledgment and subprocess CLI
coverage. `nix build .#two-host .#fleet-appliance --cores 6 --option http2 false
--option stalled-download-timeout 30` passed. Logs are retained on the persistent
VM under `/root/cas-results/runner-python-3.log` and `runner-build-3.log`.
The previous build encountered repeated cache HTTP/2 framing/partial-file errors
and stalled downloading; it was interrupted and retried with HTTP/1, preserving
completed cache entries. The initial closure-copy command failed because its
non-login shell lacked Nix in PATH; the retry explicitly sources its profile.

The ZFS arm sends actual streams from a common GUID-verified snapshot, counts
setup separately, and requires receiver success and checksum/length agreement.
The optional KVM appliance supplies a ZFS-capable kernel because native Freestyle
has no loadable ZFS module. At this commit, cross-VM guest execution remains to
validate; successful package builds and unit tests do not certify those flows.

Cross-VM attempt `native-n4-001` at `eac4bba` booted the KVM guest and completed
its verified seed write, then failed parsing fio JSON: the Nix SSH client's
warning about Ubuntu's unsupported GSSAPI option shared stdout's log. Fio now
retains stderr separately. Inspection also found that storage-only members with
no attached guests could never satisfy the runner's settle predicate; the empty
set now counts as settled. Thirteen remote Python tests pass, including both
regressions (`runner-python-4.log`). Failed guest evidence is preserved on the
owned VMs and controller; this attempt does not certify the full N=4 flow.

`native-n4-002` reached k=1 provisioning, clone verification, independently
verified drift, fleet sync and sweep, and cold-read verification, then found a
harness restart regression: the new stderr separation created unused exclusive
log files for every child, including a restarted daemon. Only fio now creates a
separate stderr file; other children retain their original combined logs.
The first retry could not find Ruff in the development shell (`runner-python-5.log`);
using `uvx ruff` explicitly passed formatting, lint and all thirteen Python tests
(`runner-python-6.log`).
The first full check overlapped closure transfers/build IO and hit five short
fixture deadlines, including a subprocess request timeout; the retry bounds
Rust test concurrency to two and retains all earlier failures.

`RUST_TEST_THREADS=2 CARGO_BUILD_JOBS=6 just check` passed remotely (541 Rust
passed, 27 ignored; 13 Python passed) at `fdc27a1` plus the separate-log fix.
The Nix formatting check found the explicit fixture case list needed formatting;
that remote formatter output is retained in the tree. The raw rsync sync arm now
also boots the synchronized clone and verifies original and drift checksums,
matching the incremental arm's destination checks. Ruff formatting/lint and all
13 Python tests pass for this revision (`runner-python-7.log`).

`native-n4-003` at `a076b84` passed guest provisioning/migration, drift sync,
sweep and verified reads for both k=1 and k=2. The first GET/PUT contention arm
then exposed a real scheduler invariant violation: `cas-host` closed the writer
with `more than one compaction IO owner`, and fio saw guest IO errors. The
compactor and shipper both use the same background class, but that class tracked
only a boolean waiter and rejected a concurrent caller. This was not a network
latency result or a capacity failure. The failed run/logs are retained.

The scheduler now counts pending background callers, consuming exactly one on
each grant or timeout. Its existing one-background/three-demand opportunity
policy, deadlines, IO ownership and allocation behavior remain unchanged. A
controlled two-background-caller regression verifies both make progress with six
demand grants and release all budget. All four scheduler tests pass remotely
(`scheduler-fix.log`). Cross-VM contention is being rerun before acceptance.
All-systems Nix evaluation passed (`runner-nix-eval.status=0`).

Closure transfer for the corrected runner initially failed twice with
`sized: unexpected end-of-file`. Explicit store export exposed the cause:
root-run Python had created `__pycache__` within the Nix experiment source,
changing its recorded NAR hash. The fleet launchers now set
`PYTHONDONTWRITEBYTECODE=1`. Only generated `.pyc` cache directories in this task's
experiment source paths were removed; `nix store verify --no-trust` confirmed the
original hash was restored (`bytecode-repair.log`). This was a deployment-artifact
failure, not a CAS hash-verification failure. Transfer logs are retained.

## 2026-11-09 — Freestyle guest, baseline and contention measurements

Native runner revision `92e194c` (clean tree), x86_64 Freestyle VMs, each 8 vCPU /
16 GiB, separate 16 GiB XFS scratch filesystems and private TCP. Daemons were
pinned to vCPUs 0–3, two-vCPU KVM workload guests to 4–5. The persistent build VM
was separate. Settings: 128 MiB image, 1 MiB seeded file plus 1 MiB deterministic
drift, 5-second read windows, QD 1/4, 4 MiB/s requested shipping writer rate,
50 µs inline polling. Native N=2 used three repetitions; these are development
smoke measurements, not physical NVMe or 100 GbE evidence.

On `cas-build`, `nix develop -c env CARGO_TARGET_DIR=/root/cas/target
CARGO_BUILD_JOBS=4 RUST_TEST_THREADS=2 just check` passed: 542 Rust passed,
27 ignored, 13 Python passed; fmt and strict Clippy passed. Final `nix fmt -- --ci`
and builds of `.#two-host` and `.#fleet-appliance` passed. Evidence:
`measure-check.log`, `measure-format.log`, `measure-build.log`, and
`appliance-final-build.log` under `/root/cas-results` on the persistent VM.
All-systems evaluation also passed earlier in this session; no CI or deployment
is inferred from these local-to-Freestyle checks.

The complete N=2 native run (`native-n2-001`) passed all six native cases. Both
placement modes provisioned and booted a verified clone, synchronized independent
drift, swept surplus, cold-read verified data, and moved the running workload by
clean shutdown, cold daemon recovery, fenced handoff and boot/verification on B.
The destination verified both files and durably advanced epoch 0→1. The full-image
rsync arm also verified the resynchronized destination by booting it.

| Native arm | Provision flow bytes | Migration flow bytes | Guest downtime (s) | Recovery/handoff or copy (s) |
| --- | ---: | ---: | ---: | ---: |
| CAS k=1 | 122,796 | 217,772 | 24.888 | 0.341 |
| CAS k=2 | 124,725 | 217,689 | 24.413 | 0.398 |
| Full-image rsync | 134,250,639 | 134,250,639 | 26.067 | 1.487 |

These are application bytes, not Ethernet/IP/SSH overhead. CAS background shipping
is **additional**: provisioning 4,381 / 8,541 bytes and migration 21,370 / 86,941
bytes for k=1 / k=2 respectively; control counters remain separate in JSON. Drift
and repair used 608,650 ship + 14,440 explicit-sync bytes for k=1, and 1,195,764 +
39,882 for k=2. Each full-image rsync sync moved 134,250,639 bytes. CAS assumes the
content is already placed in the fleet before the manifest provisioning window.

The actual common-base ZFS arm (`zfs-001`, revision `2e62822`) passed on two NixOS
KVM appliances inside two other Freestyle VMs, with KVM workload guests. Its
initial full stream established matching snapshot GUIDs and used 1,519,958
application bytes. Subsequent provision, independent-drift sync and migration
used 8,187, 1,229,206 and 7,875 bytes respectively, including stream handshake and
ACK. The migration stream took 0.231 s; snapshot/control/receive took 1.067 s in
the downtime decomposition. Total guest downtime was 124.754 s, dominated by
122.097 s of boot/verification. **Do not compare this downtime directly with the
native CAS/rsync rows:** the ZFS arm has an extra KVM layer and user-network
forwarding. The byte comparison uses actual streams; hardware timing comparisons
must run all arms natively on the same CloudLab pair. The common-base setup cost
is recorded separately, not hidden or charged to every incremental operation.

QD1 4 KiB fio total guest latency, range across three repetitions:

| Source/load | p50 (ms) | p99 (ms) |
| --- | ---: | ---: |
| Peer memory, quiet | 0.733–0.774 | 2.056–4.080 |
| Peer memory, concurrent shipping | 0.684–0.831 | 1.827–4.424 |
| Peer store, quiet | 0.807–0.848 | 2.146–2.802 |
| Peer store, concurrent shipping | 0.815–0.872 | 2.146–3.686 |
| Local raw virtual storage | 0.127–0.138 | 0.289–0.342 |

All read cohorts verified CRC32C. Quiet source labels required positive counters
and zero service from the opposite source. Every contention repetition had
6–8 sampled intervals with both verified GETs and durably acknowledged PUT chunks;
no inventory-only interval was accepted as proof. These ranges do not establish a
performance improvement under contention or the paper's peer-memory/NVMe target.
Recent-ring diagnostic median phases for quiet peer memory, in microseconds,
were receipt→send 49.45–65.75, send→first byte 356.68–429.91, first byte→verified
3.73–4.22, verified→completion 81.48–106.51. This bounded final ring is not an
unbiased whole-run sample; fio supplies the full-run percentiles. Raw JSON retains
all repetitions, histograms, achieved queue depths, throughput, counter snapshots,
wire flows, source revision, executable hashes and hardware inventory.

N=2 allocated operational footprints at the capacity checkpoints were 32,976,896
bytes for k=1, 35,414,016 for k=2, and 28,934,144 across independent stores. At this
tiny workload, bootstrap catalogs, WALs and retained metadata dominate; the result
does not establish a deduplication-capacity benefit. No metadata overhead is
silently deducted, and the independent-store control is CAS, not ZFS/DDT.

The final N=4 run (`native-n4-004`, `92e194c`) also passed all five selected cases:
k=1/k=2 guest flows, both read-source contention arms, and independent stores.
Guest downtime was 23.251 s / 22.875 s for k=1 / k=2; recovery/handoff was
0.370 s / 0.531 s. Migration flow bytes were 220,204 / 229,426, with another
55,313 / 139,830 shipping bytes. Both contention cohorts had eight overlapping
GET/acknowledged-PUT sample intervals and verified all reads. N=4 allocation
is not a matched four-guest capacity comparison; see the workload-count caveat.

**Evidence interruption:** at approximately 01:26 UTC, all five task VM IDs,
including the named persistent `cas-build`, disappeared from the provider
inventory and returned 404. This agent issued no VM deletion calls. No snapshots
were listed for those IDs. The archive had reached at least the first three host
log archives and two failed-storage archives on `cas-build`, but was lost before
export to Spark. Local summaries for N=2, N=4 and ZFS survive under
`results/freestyle/*-summary.json`; the successful test/process outcomes above
were observed directly. The latest full raw archives and build caches must not
be claimed as retained. `disappearance.json` records the five 404s. Restoration
and replacement raw evidence are in progress; pre-existing VMs remain untouched
by this agent.

Before regenerating ZFS evidence, the receiver was made to explicitly override
recordsize=4K, compression=off and atime=off, matching the source. Ordinary send
streams do not guarantee transfer of dataset properties. The earlier stream
preserved the existing file's blocks, but the destination dataset could otherwise
inherit different settings; replacement runs record both ends' properties.

## 2026-11-09 — Replacement Freestyle validation and retained raw evidence

This record supersedes the preceding restoration-in-progress status. All three
replacement suites passed and their complete controller/host archives were
exported to `results/freestyle/restored/evidence/` before deleting any replacement
test VM. The earlier lost archives remain unavailable; these are new executions,
not reconstructed evidence. No builds, tests, formatters or fixtures ran on Spark
after the owner's prohibition. Spark was used for editing, Git, provider SDK
orchestration and processing downloaded evidence only.

Native N=2/N=4 source: clean `13d4ecf4f518e51d6d6bf9c2b1aae15b77d42c78` (same
runtime as `92e194c`). ZFS and final full check source: clean
`367856b4d85cd44e164a6935d73f663416cd2ac1`. Subsequent edits only clarify ZFS
permissions and record this validation. The provider rejected the requested
larger shapes; all nine replacement VMs used x86_64, 8 vCPU, 16 GiB RAM and 64 GiB
disk. N=2 used `10.172.61.2–3`, N=4 `.4–7`, and ZFS `.8–9`; `.1` was the separate
persistent builder/controller. The suites ran concurrently on separate VM groups.
Physical provider-host isolation is unknown. Each native member used its own
16 GiB XFS scratch filesystem, daemon vCPUs 0–3 and KVM workload vCPUs 4–5.
ZFS used two nested KVM NixOS appliances because the provider kernel lacks ZFS.

Commands on the persistent VM (profile sourced first):

```sh
cd /root/cas-zfs
nix develop -c env CARGO_TARGET_DIR=/root/cas/target \
  CARGO_BUILD_JOBS=4 RUST_TEST_THREADS=2 just check
nix build .#two-host .#fleet-appliance --out-link /root/result-zfs --cores 4
# Native build/check/format used /root/cas at 13d4ecf and /root/result-fleet.
/root/result-fleet/bin/cas-two-host \
  --config /root/cas-results/native-n2-recovered-001-config.json \
  --output /root/cas-results/native-n2-recovered-001
/root/result-fleet/bin/cas-two-host \
  --config /root/cas-results/native-n4-recovered-001-config.json \
  --output /root/cas-results/native-n4-recovered-001
/root/result-zfs/bin/cas-two-host \
  --config /root/cas-results/zfs-recovered-001-config.json \
  --output /root/cas-results/zfs-recovered-001
```

Final `just check`: **542 Rust passed, 27 ignored, 13 Python passed**, with fmt
and strict Clippy passing. The ignored environment-specific tests are not claimed
as run. Passing process regressions include N=4 k=1/k=2 placement, PUT/HAS repair,
surplus sweep, provisioning, sync and migrations through a third member and back;
PUT kills before/after durability; kills at migration fence/witness boundaries;
short required-tail rejection; peer loss; and verified GET progress while a PUT
waits for durable acknowledgment. The final check includes the concurrent
compactor/shipper scheduler regression. Native build/format and final ZFS package
build/Python checks also exited zero. Earlier failed attempts and fixes are
described above; these replacement suites required no retry. No CI, origin push,
merge or physical-host power-loss result is implied.

The workload remained 128 MiB logical image, 1 MiB data plus 1 MiB deterministic
drift, requested shipping rate 4 MiB/s, five-second read windows, QD1/4 and 50 µs
inline polling. N=2 used three read repetitions; N=4 one. Both fleet modes
provisioned and booted a verified guest, synchronized independent drift, swept,
verified cold reads, and migrated with original/drift checksums and durable epoch
0→1. Rsync also boot-verified the resynchronized destination.

| Native N=2 arm | Provision flow bytes | Migration flow bytes | Downtime (s) | Recovery/handoff or copy (s) |
| --- | ---: | ---: | ---: | ---: |
| CAS k=1 | 122,813 | 217,783 | 24.490 | 0.364 |
| CAS k=2 | 126,815 | 217,699 | 22.291 | 0.427 |
| Full-image rsync | 134,250,639 | 134,250,639 | 21.989 | 1.428 |

CAS flow bytes exclude separately recorded background shipping and control.
Provisioning additionally shipped 8,541 bytes in each mode; migration shipped
38,232 / 86,908 bytes for k=1 / k=2. Provision flow elapsed time was 0.207 / 0.212 s.
Drift-and-repair windows took 1.333 / 1.399 s and transferred 600,329 / 1,187,444
shipping bytes plus 14,632 / 39,818 explicit-sync bytes. Explicit CLI timing,
control bytes and raw counters remain separate in JSON. Rsync provision and sync
took 1.510 / 1.212 s; both transferred 134,250,639 bytes. These are application
bytes, excluding TCP/IP/SSH overhead. CAS assumes preplaced content; initial
placement is outside the provisioning window. Cold boot dominates downtime, so
these runs do not show a total-downtime advantage for CAS over rsync.

Real common-base ZFS setup transferred **1,519,958 bytes**, separately recorded.
Incremental provision, sync and migration transferred **8,187 / 1,229,206 / 7,875
bytes**, with transfer times 0.284 / 0.341 / 0.259 s. Snapshot GUIDs, stream lengths
and SHA256 matched. Both datasets recorded 4 KiB records and compression off;
receive explicitly sets those properties and disables atime. Guest downtime was
133.541 s: 0.881 s shutdown, 1.140 s snapshot/transfer/install and 131.520 s
boot/verify/continue. The extra KVM layer and user-network forwarding prevent a
fair timing comparison with native CAS/rsync; all arms must run natively on the
same CloudLab pair. This validates a real incremental baseline, not a synthetic
delta estimate, and shows its small marginal transfers when a common base exists.

N=2 fio total guest latency for 4 KiB QD1, ranges across three repetitions:

| Source/load | p50 (ms) | p99 (ms) |
| --- | ---: | ---: |
| Peer memory, quiet | 0.659–0.717 | 1.499–2.671 |
| Peer memory, concurrent shipping | 0.668–0.684 | 1.581–1.761 |
| Peer store, quiet | 0.799–0.864 | 1.860–2.179 |
| Peer store, concurrent shipping | 0.815–0.922 | 2.023–3.064 |
| Local raw virtual storage | 0.146–0.153 | 0.309–0.322 |

All cohorts verified CRC32C; each contention repetition had 6–7 sampled intervals
with both verified GET progress and acknowledged PUT chunks. Quiet source checks
passed; contention retains the observed cache/store mix rather than assuming a
warm cache under write load. Quiet peer-memory recent-ring median phases were
receipt→send 46.94–50.45 µs, send→first byte 377.41–434.19 µs,
first byte→verified 2.67–3.08 µs, verified→completion 71.84–76.39 µs. The ring is a
bounded diagnostic sample, not the whole-run distribution. Sequential QD4
throughput was 21.29–24.09 MiB/s from peer memory, 16.30–19.30 from peer store,
and 64.69–65.35 from local raw virtual storage. These virtual results neither
establish the sub-40 µs target nor characterize physical NVMe/100 GbE.

N=4 k=1/k=2 guest downtime was 22.923 / 21.951 s, recovery/handoff 0.464 / 0.380 s,
migration flow bytes 220,224 / 229,219, plus 51,310 / 151,458 shipping bytes.
Memory/store contention had 7 / 8 overlapping GET/acknowledged-PUT intervals,
p50 0.651 / 0.823 ms and p99 1.499 / 2.040 ms, with verified data. N=2 allocated
footprints were 32,980,992 / 35,409,920 bytes for k=1/k=2 versus 28,942,336 across
independent CAS stores. N=4 fleet footprints were 66,682,880 / 69,152,768 bytes.
Metadata/WAL/bootstrap retention dominates this tiny workload; N=4 fleet runs two
workload guests versus four independent-store guests. These are operational
footprints, not a matched N=4 dedup ratio or a ZFS/DDT capacity comparison.

Retained evidence under `results/freestyle/restored/evidence/`:

- `checks.tgz`, `final-checks.tgz`: full remote build/check/format output and statuses.
- `native-n2-recovered-001-controller.tgz` and `-host-{0,1}.tgz`.
- `native-n4-recovered-001-controller.tgz` and `-host-{2,3,4,5}.tgz`.
- `zfs-recovered-001-controller.tgz` and `-host-{6,7}.tgz`.
- `SHA256SUMS`: digest manifest for every archive. Controller archives contain
  exact configs, source revisions, executable hashes, raw JSON and RPC logs;
  host archives contain guest/fio/daemon logs and recorded command lines.

The final-check archive SHA256 is
`542ff3307c0abc378f4f9440acefce22f1a18079fc9bf29715693f07988d5a6e`.
All eight owned replacement test VMs were deleted only after matching each local
archive digest against its remote copy; `results/freestyle/restored/deleted.jsonl`
records their IDs and digests. Unrelated/pre-existing VMs were untouched. Retained
`cas-build` (`vm-7ecd8e364c624bcb8654fa4dbd92767e`) has Nix/Cargo caches and raw
results; its VPC and named cache backup snapshot are retained too. No fixture or
build process remained on the builder at cleanup. CloudLab commands and
prerequisites are in [the runner guide](two-host-experiments.md#run-on-two-cloudlab-nodes).

Final provider availability caveat: after cleanup, copying the documentation-only
commit `657b763` to `cas-build` and directly querying that VM both returned HTTP
503 `INTERNAL_ERROR`. Its last successful exec confirmed the checks and idle
builder; current availability could not be confirmed. No delete was issued for
this VM, its VPC or cache snapshot
`sh-64d909185b34403794c97efb8d162b91` (created at 01:36 UTC). A metadata-filtered
inventory returned no matching entries, which does not distinguish provider
failure from VM disappearance. The final documentation bundle remains local;
all raw evidence and SHA256 manifests were already downloaded, so this API
failure does not affect the retained validation evidence.

## 2026-11-09 — Fixed chunk arms and memory accounting

Validated implementation revision `5b5d899bf33edeab11a5da946e72f432594b0705`
(`feat/chunk-16k`), with no uncommitted changes on the test checkout. This
record, the storage design/format descriptions and the allocation-audit TODO
clarification follow that code revision as documentation-only changes. The
interrupted work was retained and reviewed in
`cas-public/.worktrees/chunk16`, originally based on
`d49ab617808c2189cf5c36eea3e9d858ed2fb076`. No rebase onto the concurrently
changing peer branch, push, merge, CI run or deployment occurred.

All compilation, formatting, tests and fixture execution in this session ran
on the persistent Freestyle VM `cas-build-chunk16`: x86_64, 8 vCPUs, 16 GiB RAM,
64 GiB virtual disk, Linux 6.1.102, Ubuntu/glibc 2.39, Rust 1.98.1,
just 1.59.0 and uv 0.13.7. The provider rejected a 16-vCPU resize because the
account caps individual VMs at eight. No other VMs were created or modified.
Spark handled source edits, Git bundles and evidence transfer only. Build
caches remain on the named VM. Default native tests used ext4; reflink tests
and comparison data used a fresh 4 GiB XFS loop filesystem at
`/mnt/chunk16-xfs`. This is virtual-storage development evidence, not a native
NVMe measurement or a power-loss test.

### Commands and outcomes

The exact command script and environment are retained with the raw evidence.
Commands ran from `/work/cas` with the pinned toolchain and `ulimit -n 65536`:

```sh
just check
TMPDIR=/mnt/chunk16-xfs/tmp cargo test -p cas-core --locked \
  chunk16_snapshot_clone_collection -- --ignored --nocapture
TMPDIR=/mnt/chunk16-xfs/tmp cargo test -p cas-core --locked \
  reflink_snapshot_and_private_clone_roots_survive_shared_chunk_relocation \
  -- --ignored --nocapture
cargo test -p cas-core --locked --test chunk_crash -- --nocapture
cargo test -p cas-core --locked --test memory_accounting -- --nocapture
cargo run -p cas-core --locked --example chunk-arm -- \
  fixture /mnt/chunk16-xfs/comparison
cargo run -p cas-core --locked --example chunk-arm -- \
  inspect /mnt/chunk16-xfs/comparison/4k
cargo run -p cas-core --locked --example chunk-arm -- \
  inspect /mnt/chunk16-xfs/comparison/16k
mkdir /mnt/chunk16-xfs/host16
cargo run -p cas-daemon --locked --bin cas-host -- init \
  --root /mnt/chunk16-xfs/host16 --store 01010101010101010101010101010101 \
  --image 02020202020202020202020202020202=1048576 --chunk-bytes 16384
cargo run -p cas-core --locked --example chunk-arm -- inspect /mnt/chunk16-xfs/host16
```

All commands passed. `just check` passed formatting, warnings-as-errors Clippy,
554 Rust tests, eight Python tests and whitespace checks. Its default run left
28 fixture/subprocess-entry tests ignored; the two selected XFS collection
checks above passed separately. The remaining ignored fixtures and full C5
inventory were not run. No QEMU guest workload or dedicated-device comparison
was run in this session.

Coverage includes partial 16 KiB reconstruction from a pinned D plus selected
WAL data, exclusion of later writes above captured E, overlapping ZERO,
zero-padded image tails, bounded maximum batches and huge ZERO interiors,
geometry/version rejection, retained read/root pins, collection IO failures,
and recovery after three process-exit cuts for each arm (before chunks, after
chunk sync, after manifest sync). The XFS tests verify actual snapshot/clone
reflinks, collection relocation and reopen. Daemon tests cover 4 KiB reads of
16 KiB chunks, shared caching, partial overwrite, collection and restart,
peer/fleet refusal, and wrong-geometry snapshot rejection before any catalog
dependency repair. Exact counter tests check WAL headers/payload/fences, base
assembly reads, manifest preparation reads and collection rewrites without
double counting them as additional store writes.

### Development comparison

Two 1 MiB images per arm start with identical data. The second then overwrites
one 4 KiB block in every aligned 16 KiB extent. Both arms receive 2,359,296 guest
WRITE bytes. After compaction, WAL reclamation and collection, the fixture
checks every guest block against independently constructed expected bytes.
The first image still retains the original chunks, so this particular fixture
has no dead chunks to rewrite; nonzero collection traffic is checked by the
separate exact-counter and collection tests.

| Measurement | 4 KiB chunks | 16 KiB chunks |
| --- | ---: | ---: |
| Stored chunks | 320 | 128 |
| Unique payload bytes | 1,310,720 | 2,097,152 |
| Encoded / allocated chunk-file bytes after sweep | 1,343,488 | 2,121,728 |
| All root file allocations, including WAL/manifest/format | 3,321,856 | 4,026,368 |
| Live index allocation bytes | 25,112 | 12,568 |
| Index entry / table overhead / allocator slack bytes | 15,360 / 9,744 / 8 | 6,144 / 6,416 / 8 |
| Average allocated index bytes per stored chunk | 78.475 | 98.1875 |
| Guest WRITE bytes | 2,359,296 | 2,359,296 |
| WAL bytes written | 2,396,160 | 2,396,160 |
| Store bytes appended | 1,343,488 | 2,121,728 |
| Manifest bytes written | 139,264 | 65,536 |
| Compaction read bytes | 2,416,640 | 5,275,648 |
| Collection rewrite bytes (subset of store writes) | 0 | 0 |
| Software write ratio | 1.644097 | 1.942708 |
| Compaction read ratio | 1.024306 | 2.236111 |

The software write ratio is `(WAL + store appends + manifest writes) / guest
WRITE bytes`. It excludes root-format creation and filesystem/device metadata
traffic. Collection rewrites are already in store appends. All-root allocation
includes retained WAL headers, so it is distinct from unique stored payload.
Offline inspection rebuilt the same index allocation totals in this fixture;
in general collection retains table capacity and a rebuild can differ. Libc
usable-size slack may also vary between equal layouts. These are one-run,
small-fixture values, not constant marginal bytes per entry or scaling results.

The 16 KiB arm saves index allocation here but loses aligned 4 KiB deduplication
when neighboring blocks differ, and reads untouched base data to reconstruct
partial extents. No ZFS arm, device write counter, equal-total-memory control,
CPU-per-GB result, guest latency histogram or repeated-trial variance is claimed.
Those paper measurements remain open.

### Memory evidence and remaining audit

The isolated memory test touches 110,000 index entries, an 8 MiB clean payload
cache and a 4 MiB manifest payload cache. Actual component charges were:
index 6,426,608 bytes; clean cache 9,515,256; manifest cache 4,669,672; total
20,611,536. Measured RSS increased from 3,764,224 to 32,489,472 bytes, a delta
of 28,725,248. Libc live allocation increased by 20,655,872 and free arenas by
8,048,896 bytes. The RSS residual after component charges and free arenas was
64,816 bytes. RSS after dropping the components was 4,354,048 bytes. The test
checks allocation reconciliation within 512 KiB and RSS reconciliation within
2 MiB; it does not equate usable allocation bytes with process RSS.

The index ledger charges full hashbrown layout plus `malloc_usable_size` slack,
including simultaneous old/new tables during growth. Cache totals include their
tables, shared controls and reader-held values. Allocation-recorder tests
independently check Arc layout, final-deallocation credit ordering and rejected
slack charges. Offline inspection explicitly reports live cache allocations as
unavailable; live host telemetry exposes clean and manifest cache totals.

The allocation-audit TODO remains unchecked. This work covers these component
allocations, not all runtime allocations: request/fetch/recovery buffers,
compaction scratch/preparation, diagnostics, fixed ledger objects, allocator
bookkeeping/retained arenas, stacks, libraries and kernel/page-cache memory still
need a complete daemon-lifetime reconciliation and pressure evidence. Cache
payload caps are not equal-total-memory controls.

### Failures, retries and retained evidence

Initial `cargo test -p cas-core --locked` passed on the reviewed dirty
`d49ab61` tree. The first complete `just check` failed 21 peer/fleet tests with
`TooManyOpenFiles` and disconnected children under the VM's 1,024-descriptor
soft limit. Raising the limit to 65,536 made the next complete check pass.
A later check at `0d995c1` exposed an existing index-growth test assumption:
equal layouts happened to get different libc usable sizes (observed peak 624
versus expected 640 bytes). `5b5d899` checks the actual old/new allocations of
each attempt and their exact overlap; the final complete check passed. A
metadata-capture script initially stopped on `ldd --version | head -1` under
`pipefail`, before any final tests; retaining full version output fixed it.
One stalled source upload was canceled and retried with a compressed archive.
The interrupted attempt's older logs were preserved but are not acceptance
for this revision.

Raw logs, JSON, environment and command script are under ignored
`results/chunk16-resumed/evidence/`, with the same files at `/work/evidence` on
`cas-build-chunk16`. Fixture files remain at `/mnt/chunk16-xfs/comparison` and
`/mnt/chunk16-xfs/host16` on that VM. The transferred code history is retained
there and locally. `evidence.tgz` SHA-256 is
`a9b9d0a367ca168890f8aaa684d0545a4c28dd571560b09ff03df5f646587b10`.
The first dirty-source archive is `results/chunk16-resumed/first-tested-source.tar`,
SHA-256 `d4321f3500f1f2d5958aebe93ca869b479a3ce7fdc7113736723ddd44357105f`;
`inherited.patch` preserves the tracked changes found at resumption.
