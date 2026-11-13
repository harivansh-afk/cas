# Validation

## 2026-11-13 — Integration of the burst harness and the CloudLab campaign driver

On `integration/2026-11-13` after the measured-flags/importer/raced-read
integration and its follow-ups (`9b80a02`): merged, with merge commits,
`feat/burst-harness` at `4565a0d` as `da90544` and `feat/cloudlab-campaign`
at `0e9c5a2` (branched from the former) as `1e09998`; then `6d5a7d6` makes the
campaign's pinned revision a run-time input and `607f051` makes a new
test's clock bracket inclusive. **Tested revision: `607f051cd550e5d924244931d1df180e181de205`**,
clean on the VM. This record is a later documentation-only commit; nothing
was pushed or merged into `main`. This head is the candidate measured
revision: a clean tree with no revision value committed into it.

### Merge resolution

- `experiments/two-host/runner.py`: the harness's settings defaults
  (`guests_per_host`, `guest`, `working_set`, `burst`, `resume`) kept beside
  this branch's `peer_race_us` / `fast_settled_reads`; `test_runner.py`
  keeps both test classes; `agent.py` keeps the campaign's
  `working_set_script` beside this branch's `measurement_flags`, which still
  builds the `cas-host` flags for every fleet host. The campaign's
  `campaignlib.RUNNER_SETTINGS` names exactly those two settings, so its
  "runner supports the arm's settings" check now passes for the `fast` and
  `raced` arms.
- `crates/harnesses` (native N guests, ZFS raw storage) merged without
  conflict.
- `docs/validation.md`: every entry kept, newest first; the campaign's
  revision of the burst-harness entry taken.
- `6d5a7d6` (coordinator decision): `expected_revision` is a run-time input
  (`campaign.py --revision`, else `expected_revision` in a nodes file kept
  outside the tree), required unless the loopback development layout,
  verified against every node's checkout at `run` and recorded in the
  manifest. `INTEGRATION_REVISION_PLACEHOLDER` is gone from the library, the
  example and the doc; no self-referential pin commit exists.

### Host and commands

Fresh Freestyle VM `cas-build-integration` (`vm-ad8d75225752469cb239c4ae6a4fbd6d`, same
shape, Nix and procedure as the entry above), deleted after export. Spark
ran only the Python unit suites (two-host 39, campaign 28) and Git.

```sh
nix develop -c just check
nix develop -c cargo test -p cas-harness --locked
nix develop -c uv run --no-project python -m unittest discover -s experiments/cloudlab
nix develop -c uv run --no-project python -m unittest discover -s experiments/two-host
bash experiments/cloudlab/setup.sh --bundle /root/cas.bundle --revision 607f051cd550e5d924244931d1df180e181de205 \
  --source /root/cas-campaign --skip-disk --skip-irq --skip-nix --cores 6 --out /root/campaign/setup.json
python3 experiments/cloudlab/campaign.py dry-run --nodes nodes-dry.json \
  --plan experiments/cloudlab/plans/dev-loopback.json      # loopback layout, two loop-XFS members
```

### Outcomes

- `just check` on `607f051`: **602 Rust tests passed, 0 failed, 30
  ignored**, 39 Python tests (two-host); `cargo test -p cas-harness`: **60
  passed, 0 failed**; campaign unit tests **28 passed**; two-host unit tests
  **39 passed** (also on Spark). A first `setup.sh` build on `6d5a7d6`
  failed in the `cas` package's sandboxed release tests: the new ring-origin
  test's strict `before < received` bracket met an equal reading;
  `607f051` makes the bounds inclusive (nothing else changed) and the build
  passed.
- `setup.sh` (`--skip-disk --skip-irq --skip-nix`): bootstrap clone from the
  bundle, checkout at the pinned revision, `nix build .#two-host` and
  `.#native` (both from source on the fresh VM, the `cas` package's release
  tests included), limits and inventory: exit 0.
- `campaign.py dry-run` with `plans/dev-loopback.json` on a two-member
  loopback layout (`loopback-8` node type, two 8 GiB loop-XFS mounts, one
  shared `output_root`, no pinned revision as the layout allows): **5
  runnable, 7 blocked of 12 units, exit 2, nothing launched**; manifest
  bound to `607f051` and the binaries' hashes, equal on both members;
  evidence label `development`. Runnable: `frozen reads-memory` ×2, `fast
  reads-storage` ×2 and `raced reads-storage` — the `fast` and `raced` arms
  are no longer blocked for "runner does not know the setting", because the
  integrated runner carries `fast_settled_reads` / `peer_race_us`. Blocked
  for the plan's intended reasons: `fast reads-storage-g8`, `native-cas g4`
  and `burst burst-images` (8/8/16 guest cpus exceed the loopback set of 4;
  `cas-native-run` boots at most 2 guests), `native-raw g1` and `native-zfs
  g1` (no native runner path / no ZFS dataset in this nodes file), `r0
  floor` (nvme-tcp prerequisites absent on the VM) and `resume
  resume-elsewhere` (`WORKING_SET_FROM_BUNDLE` needs `bundle_actions`).
- Open for a human: the dry-run was a loopback development layout; no
  campaign unit ran here, and the native (`cas-native-run`) wrappers were
  built but not exercised. The loop-XFS images were unmounted before the
  VM was deleted (they had stalled the previous VM's IO, see the entry
  above).

Evidence: `results/integration-1113/chain-merge.log` and
`evidence/evidence-3.tgz` (logs, setup and dry-run outputs) with its SHA-256
in `evidence/export-3.log`; VM ledger `resources.json`.

## 2026-11-13 — Integration of measurement flags, offline import and raced reads

Branch `integration/2026-11-13` from `main` (`35add83`), merged in order with
merge commits (no squash, no rebase): `feat/measure-flags` at `cbe027f` (code
`39214ec`) as `388ab56`, `feat/offline-import` at `77c4081` as `1593fd2`, and
`feat/raced-read` at `41bf61f` as `4873e1a`. One follow-up commit `7d55d8a`
adds the race counters to a test's assertion message (test-only). **Tested
revisions: `4873e1a`** (`just check`, flagged suites, importer) **and
`7d55d8a`** (probes, loopback, C5; identical runtime code), clean on the VM.
This record is a later documentation-only commit. Nothing was pushed or merged
into `main`. These are Freestyle development checks, not CI, paper or
native-hardware results.

### Merge resolution

The first two merges were clean apart from `docs/validation.md`. The third
conflicted in `peer/client.rs`, `peer/router.rs`, `peer/tests.rs` and
`docs/validation.md`; `local/fast.rs` auto-merged but no longer compiled.
Every resolution keeps both features' invariants:

- `peer/client.rs`: the raced-read `Slot` (generation, notify, challenger,
  pending race, racing/used/wire bytes) gains measure-flags' `image` slot
  index. `Client::fetch_with` and `Shared::fetch` carry `image`;
  `Client::fetch` passes `UNATTRIBUTED`. `Pool::finish` calls
  `Metrics::transport_finished(slot.image, verified)` (host `verified`/`errors`
  plus the issuing image's `verified_bytes`) in place of the plain counter
  update, keeping raced-read's failed-primary challenger start and `notify`.
- `peer/race.rs`: `PendingKind::start` takes the primary's image, so a remote
  challenger GET is credited to the same image as its primary; `Slot::start_pending`
  passes `self.image`.
- `peer/router.rs`: raced-read's `Shared` (placement, clients, local source)
  also holds the `Racer`, so measure-flags' per-image `Router::for_image`
  views share connections, racer and local source; `Router` is
  `{shared, budget_us, image}`, `fetch`/`fetch_with_notify` pass `self.image`.
  `Router::new` keeps raced-read's fourth argument (scheduler port for the
  local race source).
- `local/fast.rs`: `Context::peer_metrics` is `Option<ImageMetrics>`
  (measure-flags' `Port::peer_metrics()` type); the inline path's `Trace`
  therefore attributes its reads to the image like the reactor's. Its GETs go
  through the image's router view, so inline fetches are credited at
  transport verification exactly like reactor fetches.
- `peer/tests.rs`: `Metrics::new(&budget, 1)` and `Router::new(.., None)`.
- `docs/validation.md`: all five branch entries kept, in date order (three
  2026-11-13 entries, then two 2026-11-12), above the 2026-11-09 entry.
- Attribution decision for a race won by the local copy: there is no transport
  verification, so no `verified_bytes` and no `verified`; the read counts as
  completed with its chunk on the issuing image, and the late remote reply,
  when it verifies, is still credited once to the issuing image and to
  `counters.verified`. The sum `Σ images.verified_bytes = 4096 × counters.verified`
  therefore holds with races, with coalescing and with inline reads. Written
  into `docs/peer-fetch.md` (`peer.images` paragraph; `verified`/`errors`
  count remote replies only).
- Tests added at the merge: `assert_verified_bytes_sum` in
  `local/host/tests/peer.rs`, asserted in the per-image, coalescing,
  local-win, inline-registry and both loopback tests; the local-win test
  asserts image 2 = {8 reads, 8 chunks, 8 × 4096 verified bytes}; the
  inline-registry test asserts both inline GETs credit image 2; the
  router-level second-owner race test fetches through `for_image(0)` and
  asserts the challenger's win and the late loser's reply are both credited
  to that image (`2 × 4096`). The peer test fixture now arms `CAS_PEER_RACE_US`
  on every host that fetches (like the fleet fixture); `CAS_PEER_RACE_LOCAL`
  stays test-explicit because it changes the force-remote failure semantics.
- Docs: `docs/casctl.md` gains a flags table (every new `cas-host` and
  `casctl import` flag with its default; no `cas-host` default changed);
  `docs/peer-fetch.md` "Read path" now describes the two schedulers (reactor,
  frontend-inline) over the shared per-block resolution in `local/chunk.rs`
  and points to `docs/storage-design.md` for the inline conditions.

### Host and commands

No build or test ran on Spark (editing, Git bundles, provider calls and
evidence reading only). Freestyle x86_64 VM `cas-build-integration`
(`vm-ae4098253ad34f30867b5890f5702762`, snapshot `freestyle/ubuntu-lg`,
Ubuntu 24.04.5, Linux 6.1.102, 8 vCPU, 16 GiB, 64 GiB ext4 root, `/dev/kvm`
present), created for this task with its own identity grant and deleted after
export. Nix 2.35.6 (upstream multi-user installer; `nix.conf`: flakes,
`max-jobs = 2`, `cores = 8`), `nix develop` Rust 1.98.1. The branch was
shipped as a git bundle; `ulimit -n 65536`, `CARGO_TARGET_DIR=/root/target`,
`CARGO_BUILD_JOBS=8`. Everything ran as root, sequentially.

```sh
nix develop -c cargo fmt --all -- --check && nix develop -c cargo clippy --workspace --all-targets --locked -- -D warnings
nix develop -c just check
T="nix develop -c cargo test -p cas-daemon --locked --lib --"
$T peer;  CAS_FAST_SETTLED_READS=1 $T peer
CAS_PEER_RACE_US=100 CAS_PEER_RACE_LOCAL=1 $T peer
CAS_PEER_RACE_US=100 CAS_PEER_RACE_LOCAL=1 CAS_FAST_SETTLED_READS=1 $T peer
$T fleet; CAS_FAST_SETTLED_READS=1 $T fleet; CAS_PEER_RACE_US=100 $T fleet
CAS_PEER_RACE_US=100 CAS_FAST_SETTLED_READS=1 $T fleet
$T local::host::tests::fast; CAS_PEER_RACE_US=100 CAS_PEER_RACE_LOCAL=1 $T local::host::tests::fast
$T local::host::tests::recovery::service
nix develop -c cargo test -p cas-core --locked --test import --test import_crash
CAS_PEER_EVIDENCE=... nix develop -c cargo test --release -p cas-daemon --lib --locked \
  local::host::tests::peer::two_daemon_loopback_verifies -- --nocapture   # 3× per arm
nix build .#checkpoints --out-link result-checkpoints; nix build .#cas --out-link result-cas
nix develop -c result-checkpoints/bin/cas-checkpoints --checkpoint C5 --checkout . --output /root/c5/suite-01
result-cas/bin/cas-harness verify-suite --output /root/c5/suite-01
```

### Outcomes

- `cargo fmt --check` and strict Clippy: clean on the first compile of the
  merged tree.
- `just check` on `4873e1a`: **pass** — **593 Rust tests passed, 0 failed,
  30 ignored** (579 on `feat/measure-flags`, 583 on `feat/raced-read`, 577 on
  `feat/offline-import`'s base; the counts add up less the shared base plus
  the merge's own assertions), 13 Python tests, `git diff --check`.
- Peer suite (`peer` filter: `peer::tests`, `peer::wire::tests`,
  `local::host::tests::peer`, plus two `recovery` tests and two fleet/chunk16
  tests whose names contain "peer"; 31 selected, 2 ignored entry points):
  flags off **29 passed** (first run 27/2, see failures), fast **31 passed**,
  race (`--peer-race-us 100 --peer-race-local`) **29 passed** (first run 28/1),
  both **31 passed**. The per-image attribution and coalescing tests and the
  sum assertion passed in all four arms.
- Fleet suite (`fleet` filter, incl. the two- and four-process fixtures):
  **20 passed, 1 ignored** in each of off, fast, race (`CAS_PEER_RACE_US=100`)
  and both.
- `local::host::tests::fast`: 2 passed (off and race+local);
  `recovery::service`: 4 passed, 2 ignored (XFS/subprocess).
- Importer: `tests/import.rs` and `tests/import_crash.rs` passed (both chunk
  arms, every crash cut).
- Release loopback (below): 12/12 runs passed, delayed-server 2/2.
- C5: **41/41 passed; the independent verifier passed** (exit 0, silent).
  Suite 04:21:55–05:17:25 UTC (55.5 min, exit 0) on `7d55d8a` recorded by the
  harness (`revision/`), default flags, as root on the VM's nested KVM; no
  scenario was retried. `nix build .#checkpoints` and `.#cas` (release
  package tests) passed first (04:16–04:21). `guest-pressure` counted 2
  staging stops (`stops` 2 in the write-stage daemon telemetry; 2 of its 592
  one-second samples caught `stopped`). No scenario was infeasible on the VM.

| Scenario | Result | Seconds |
|---|---|---:|
| `async-after-active` | PASS | 17.91 |
| `async-after-append-cqe` | PASS | 16.85 |
| `async-after-prepared` | PASS | 17.52 |
| `async-after-status` | PASS | 17.23 |
| `async-after-storage` | PASS | 17.04 |
| `async-after-sync` | PASS | 17.25 |
| `async-after-used` | PASS | 17.42 |
| `async-before-submit` | PASS | 16.59 |
| `async-before-sync` | PASS | 16.53 |
| `async-device-reset` | PASS | 19.13 |
| `async-fresh-recovery` | PASS | 38.75 |
| `async-io` | PASS | 37.67 |
| `async-repeat-after-fence` | PASS | 17.23 |
| `async-repeat-before-fence` | PASS | 17.23 |
| `async-repeat-replay` | PASS | 17.65 |
| `clippy` | PASS | 2.17 |
| `diff-check` | PASS | 0.02 |
| `fresh-recovery` | PASS | 36.28 |
| `guest-pressure` | PASS | 472.72 |
| `live-after-status` | PASS | 19.51 |
| `live-after-storage` | PASS | 17.69 |
| `live-after-used` | PASS | 17.98 |
| `live-before-submit` | PASS | 18.33 |
| `local-fresh-recovery` | PASS | 38.50 |
| `local-sync` | PASS | 40.02 |
| `nix-check` | PASS | 147.31 |
| `nix-format` | PASS | 4.23 |
| `persistence-model` | PASS | 12.95 |
| `raw-daemon` | PASS | 23.89 |
| `raw-qemu` | PASS | 18.85 |
| `rust-format` | PASS | 0.97 |
| `rust-tests` | PASS | 73.61 |
| `shared-after-chunks` | PASS | 250.61 |
| `shared-after-d` | PASS | 250.45 |
| `shared-after-manifest` | PASS | 260.86 |
| `shared-after-punch` | PASS | 256.29 |
| `shared-after-unlink` | PASS | 265.05 |
| `shared-before-chunks` | PASS | 227.77 |
| `shared-replacement` | PASS | 242.43 |
| `staging` | PASS | 25.55 |
| `xfs-store` | PASS | 284.93 |

### Release loopback, three runs per arm

Same harness and method as the `feat/raced-read` entries (release, QD1,
4 KiB, 64 chunks, 100 warmup + 600 measured reads, nearest-rank quantiles over
ring ids 100–699, microseconds, ranges over three runs). Different VM
instance; compare arms within this table only. Development evidence, not a
paper result.

| Arm | Receipt→send p50 | Receipt→send p99 | Verified→completion p50 | Verified→completion p99 | Total p50 | Total p99 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Flags off | 24.8–27.8 | 39.4–47.9 | 37.2–40.4 | 53.7–58.6 | 103.0–109.5 | 136.0–149.0 |
| `--fast-settled-reads` | 8.4–13.1 | 16.3–30.9 | 15.8–16.1 | 31.6–33.2 | 62.4–89.7 | 97.0–131.7 |
| Race 100 µs + local, fast off | 23.5–23.9 | 42.6–52.5 | 31.8–33.8 | 55.9–70.3 | 100.1–114.8 | 152.0–170.8 |
| Race 100 µs + local + fast | 9.5–10.3 | 23.1–30.5 | 14.5–16.8 | 32.4–34.9 | 63.3–89.2 | 98.1–153.8 |

Every run: 700 GETs, 700 verified, 0 errors, cache empty, and
`Σ images.verified_bytes = 4096 × verified` in the measure, pipeline and
post-loss reports. Race arms started 13–22 (race) and 7–27 (both) races per
700 reads with 1–10 local wins, one discard per start, `race_dropped` 0,
`race_won_second` 0 (two-host configuration: the only challenger is the
local copy). Delayed server (1 ms reply, 100 µs budget): 700/700 local wins
with and without the fast flag, `race_discarded` 700 once the late replies
had landed, `race_wasted_wire_bytes` 2,884,000, `errors` 0; total median
419.3 µs (fast off) / 405.0 µs (fast on). The sum invariant also held on the
snapshot taken before the late replies arrived (258 and 244 verified).

### Failures and retries

1. First `peer` run, flags off: 27 passed, 2 failed —
   `recovery::frontend::retained_collection_does_not_repair_before_all_inputs_and_abandonment_fails_peers`
   (`Store` reopen `WouldBlock` in the test helper, `tests.rs:195`) and
   `recovery::service::failed_retained_negotiation_cancels_an_unconnected_peer_without_repair`
   (`frontend connection: No such file or directory` after the helper's 5 s
   connect deadline). Both tests are unchanged from `main`, ran in parallel
   with the subprocess-spawning loopback and descriptor-limit tests, and
   passed in `just check`, standalone 3/3, and in two repeated `peer` runs
   (29/29 each). The second failed again once in a repeated race-arm `peer`
   run (28/1; 5.4 s run time versus 2.6 s when passing), so it is a
   timing-sensitive test under this filter's concurrency: on `main` (`35add83`, same VM, own target
   directory) the same filter selects 18 tests and passed 5/5; the merged tree
   selects 31, adding the raced/inline loopback and descriptor-limit tests that
   fork subprocesses. The likely mechanism is the one recorded on 2026-11-09
   (failure 2): a child between fork and exec shares every open description,
   so a concurrent test's `flock` sees `WouldBlock`; `host_service::serve`
   failing its open that way before binding the socket would explain the 5 s
   connect expiry (the test panics on the connection before reading the
   service result, so the error is not retained). Not shown; left for a human:
   extend the one-second lock retry to that path, or serialize the
   fork-spawning tests. No bound or assertion was changed.
2. First `peer` run, race arm: 28 passed, 1 failed —
   `two_daemon_loopback_race_wins_locally_against_a_delayed_server` saw
   `races_started` 699 of 700 (debug build, parallel suite). It passed 5/5
   standalone under the same environment, in the repeated race-arm runs, in
   the both arm, and in the release delayed-server runs (700/700 twice). The
   assertion now prints the counters (`7d55d8a`) so a recurrence shows which
   counter moved; no bound was changed.
3. No other failure; no rebuild was needed after the merge.

### Evidence and limits

Ignored research-repository root `results/integration-1113/`: `vmctl.py`,
`run.sh`, `suites.sh`, `probe.sh`, `loopback.sh`, `c5.sh`, `summarize.py`,
the VM ledger `resources.json`, `nix-install.log`, `warm-build.log`,
`compile1.log`, `suites.log`, `probe.log`, `loopback.log`, `c5.log`,
`loopback/` (per-arm `reader.json`, `pipeline.json`, `server.json`,
`failure.json`; delayed-server `race/reader.json` and `race/late.json`) and
`evidence/evidence.tgz` (every VM log, the loopback JSON and the C5 suite
directory without disk images) with its SHA-256 in `evidence/export.log`.
The VM `vm-ae4098253ad34f30867b5890f5702762` was deleted through the provider
API after export (ledger event `vm-deleted`); no other VM was touched.
`evidence/evidence.tgz` SHA-256
`2a627d5441e778750ccaa1473e5354c6fd18bcfa986ca2819372dfce8359a0a2` (21 MB;
`xfs.img`, raw images, WAL segments and staging logs excluded);
`evidence/c5-suite-01.json` is the suite receipt. A `main` checkout and its
target directory were built on the VM only for the flake probe. Limits: one
C5 run, as root, on nested KVM in a shared-provider VM; loopback timings are
development observations on that VM (no pinning, no IRQ control) and exclude
guest scheduling; no CI, push, merge, deployment, CloudLab, multi-host
(`cas-two-host`) or guest-measured run occurred in this session. The
multi-host N=2/N=4 suites from the 2026-11-09 entry were not repeated here.

### Follow-up (same day): flake fix, review fixes, multi-host suites

Coordinator follow-ups on the same branch; **tested revision `57acfcd5d67f690787ac9d5bca8f32710f672561`**
(commits `1111965`…`57acfcd`), clean on a second fresh Freestyle VM
`cas-build-integration` (`vm-0b06f9f7927c4a8093127b61daa71248`, same shape,
Nix and procedure as above; the first VM had been deleted after the entry
above). The VM was deleted after export. Nothing was pushed or merged.

#### The `peer`-filter flake: two causes, both fixed at the source

Reproduction on the unmodified head `15754ae`: `cargo test -p cas-daemon
--lib -- peer` ten times → **4/10 failed** (`failed_retained_negotiation…`
three times with `host.json` `startup_error: "operation would block"`, i.e.
`host_service::serve` failed at the root lock before binding any socket, and
`race_completes_from_the_local_copy…` once). Skipping the one test that
spawns a subprocess with a `pre_exec` hook → **0/10 failed**; skipping the
`posix_spawn` loopback tests instead → **3/10 failed**. With a `pre_exec`
hook `Command` must `fork()` the large multi-threaded test binary, and
between fork and exec the child holds a copy of every open description, so a
`flock` another test has just released stays held for milliseconds (locks
live on the description). Fix (`1111965`): the descriptor-limit child lowers
its own `RLIMIT_NOFILE` from the environment and `Command` uses
`posix_spawn`. A first attempt at lock-holder diagnostics was wrong twice
(hex inode match against the decimal `/proc/locks` field; the "holder" it
printed was its own contender descriptor) and its `/proc/*/fd` scan was slow
enough to distort the retry budgets; that probe was discarded. The 10/10
failures recorded while it was in place were mostly a second mistake of
mine: the `try_connect` helper had dropped the feature/protocol negotiation,
so the service never saw the inflight request; fixed before any kept run.

With the fork window gone the `WouldBlock` still recurred once in 14 runs
(`Log::create` in a test helper). Second cause: `geometry::config` read the
immutable store marker `chunk-format.v3` through `direct::open`, which takes
an **exclusive** `flock` for a transient read-only check; every WAL open
(`append/shared.rs`) and every store inspection reads it, from several
threads of one daemon and from fixtures beside it, so concurrent readers of
one root collided. Fix (`4ba5520`): readers take a shared lock
(`direct::open_readonly`); `create` keeps the exclusive one; unit test.

A read-path bug surfaced by the same loops (`3ff5d42`): both schedulers armed
the race only while the primary ticket was not `ready()`; a failure that
landed first (refused connection, EOF, a corrupt payload verified on the
worker thread) made the ticket ready, so the read was never raced and failed
closed although the local copy or second owner would have served it —
intermittent `ConnectionRefused` and `races_started 0` in the race tests.
`Ticket::verified` now distinguishes a verified reply (nothing to race) from
a failed one (raced; arming an already failed primary starts the challenger
at once). Two timing assumptions in tests were widened to what a loaded
debug host can honour: router-level race budgets 5 ms → 100 ms against
500 ms late replies, and the delayed-server loopback test's server delay
1 ms → 10 ms (`c463a3c`).

Loops on the fixed tree (debug, parallel, 8 threads, `peer` filter = 32
tests incl. the loopback subprocess tests): after the fork fix and the
shared-reader change, 12/12 then 15/15 (ten flags off, five race arm) and
three full daemon-lib runs; the full campaign on `97575b8` added 10/10 off +
5/5 race with one `WouldBlock` in a `Log::create` of the parallel loop
(1 in about 40 runs), traced to the marker's *creator* still taking an
exclusive lock (`11c2f57`); 20 runs on that head: 18/20, the two misses
being the delayed-server test counting a legitimately unraced read
(`d94f345`); 20 runs on `d94f345`: 19/20, the miss a run in which two
unrelated tests timed out together (11.7 s wall versus 2.7 s; a stall of
the shared VM, not a code path); 10 runs on `57acfcd` (rustfmt of that test hunk): 10/10. The `just check`
of that loop failed with eleven unrelated tests at their 5 s request
deadline in one run and the daemon-lib binary at 110 s instead of 44 s;
`dmesg` showed hung `xfs-sync/loop*` workqueues (`xfs_log_worker` pending)
from the four 16 GiB XFS images loop-mounted for the multi-host runs,
which also explains the loop-10 miss. With those images unmounted and
detached, `just check` passed twice in a row on `57acfcd` (**599 passed, 0
failed, 30 ignored**, 14 Python tests). Three timing assumptions in
the race tests were corrected rather than tolerated along the way: the
corrupt-reply test asserted the reply's error before its discard (the local
copy can win before the transport has connected); the delayed-server test's
"700/700 local wins" is a timing claim the debug parallel suite cannot
honour (half the reads lost to the 1 ms reply under load once), so the 95 %
floor applies to release builds and the debug build asserts exact accounting
(wins sum to starts, one discard per start, wasted bytes equal local wins ×
frame, 700 verified, 0 errors, unraced reads counted); the failed-primary
test's 150 ms wall-time guard tripped at 158 ms and is now a loose guard
against a 2 s budget. A 10 ms server delay was tried and rejected: 700
serial replies back up past the 2 s GET timeout.

#### Review fixes (nine findings)

- `67c5cd6` (1) `SlotRef::due` on a generation mismatch no longer takes the
  newer occupant's `pending`; test `stale_racer_deadline_does_not_cancel_a_reused_slots_race`.
- `5c84944` (2) `fast.rs` fails the shared host gate only for failures of
  shared IO (poll-time: a GET reply, a waited publication, the cache fill);
  local exhaustion while joining fails only the read and its image. Test
  `registry_exhaustion_after_a_waiter_joined_fails_only_the_image`.
- `1905ffc` (3) `validate_interval` refuses a segment too small for one WAL
  batch instead of underflowing; unit test at and below the floor.
- `5c84944` (4) one fetch-registry state machine in `local/chunk.rs`
  (`Sources::join`, `Fetch::publish`) used by the reactor and the inline
  path; (5) `decline` keeps the `Io`; (6) `cas_core::eventfd::duplicate` is
  the one descriptor duplication, transport notifies are `OwnedFd`; (8)
  `receive(wait)` with pending inline reads blocks on the frontend eventfd
  (the reactor already signals it after every mailbox send); (9) the FLUSH
  table overflow counts `fast.flush_overflow` and logs once.
- `c2b30ad` (7) image keys through `cas_core::id::hex`.
- `29da053` two-host runner settings `peer_race_us` / `fast_settled_reads`
  (needed for the "both flags" multi-host arm; unit test).
- `1389b8a` ring origin: `peer.origin_monotonic_ns` and
  `fleet.shipping.origin_monotonic_ns` are the `CLOCK_MONOTONIC` reading taken
  with the ring origin, so a ring time `ns` is the host instant
  `origin_monotonic_ns + ns` (documented in `peer-fetch.md` and `fleet.md`;
  tests: reads stamped after a known instant convert to later instants
  bounded by now; the shipping ring shares the origin). Ring entries are
  unchanged.
- `7295c9c` fast path: inline completions signal the frontend completion
  eventfd (found by the multi-host runs below).

#### Outcomes on `57acfcd`

- fmt, strict Clippy: clean. `just check`: **599 Rust tests passed, 0
  failed, 30 ignored**, 14 Python tests.
- Peer suite (`peer` filter, 32 selected): off **32**, fast **32**, race
  **32**, both **32** passed (2 ignored entry points each); fleet: **20/1
  ignored** in all four arms; `local::host::tests::fast` 5/5 (off, race);
  importer 2/2; `peer` filter loops: 9/10 off (see below), 5/5 race.
- Release loopback, three runs per arm (same method as above; this VM):

| Arm | Receipt→send p50 | p99 | Verified→completion p50 | p99 | Total p50 | Total p99 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Flags off | 21.8–27.3 | 37.9–65.1 | 31.1–36.3 | 49.7–78.6 | 96.3–119.8 | 146.5–208.6 |
| `--fast-settled-reads` | 8.1–12.4 | 20.2–26.6 | 13.7–19.0 | 28.5–37.9 | 55.3–74.1 | 93.4–109.6 |
| Race 100 µs + local, fast off | 22.7–32.0 | 37.5–47.9 | 29.2–39.1 | 53.5–54.0 | 93.0–124.6 | 139.7–161.0 |
| Race 100 µs + local + fast | 8.7–14.8 | 16.8–31.7 | 16.6–22.4 | 31.0–43.0 | 72.5–93.1 | 103.5–123.0 |

  Every run 700 verified, 0 errors, `Σ images.verified_bytes = 4096 ×
  verified`; race arms 11–18 starts per 700 reads (5–13 local wins), one
  discard per start, `race_dropped` 0; `peer.origin_monotonic_ns` present.
  Delayed server (1 ms reply, 100 µs budget, release, standalone):
  **700/700 local wins** with and without the fast flag, `race_discarded`
  700, wasted 2,884,000 bytes, 0 errors; total median 479.8 µs (fast off) /
  406.6 µs (fast on).

#### Multi-host N=2 and N=4

The two-host runner ran on this VM with every member local (as the N=4 run
of the 2026-11-09 entry did on one of its VMs): members on separate 16 GiB
XFS loop mounts `/mnt/fleet0..3`, GET/bulk ports 17483–17490 on 127.0.0.1,
daemons pinned to vCPUs 0–3 and guests to 4–7, nested KVM guests, settings
as the 2026-11-09 run (128 MiB image, 1 MiB data, 4 MiB/s shipping, 5 s
windows, QD 1/4, one repeat); N=2 all six cases, N=4 the five
(`raw-rsync` is two-node). The dev shell's `mkfs.xfs` 6.19 wrote feature
bits (`0xc0`) kernel 6.1 rejects; Ubuntu's xfsprogs 6.6 with
`-m rmapbt=0,inobtcount=0 -i nrext64=0` mounted. `cas-two-host` gained
`settings.peer_race_us` / `fast_settled_reads` (`29da053`) so the "both
flags" arm passes `--peer-race-us 100 --fast-settled-reads` to every fleet
host.

**First attempt (tree `48f36e5`):** flags off, N=2 and N=4 both passed
`flows-k1`, `flows-k2`, `reads-memory` and failed in `reads-storage`
(N=2: the contention writer's fio SSH call exceeded its 260 s timeout; N=4:
`r0-contention-writer` fio exit 1); **both flags on, N=2 and N=4 failed the
very first guest boot** (`flows-k1` seed: "experiment readiness deadline").
Attribution with `flows-k1` alone: `fast_settled_reads` only → the same
boot stall; `peer_race_us` only → pass. Cause (`7295c9c`): a vhost-user
backend learns of completions only through its completion eventfd (the
reactor writes it after every mailbox send, a GET reply writes it from the
transport); a read the fast path completed inline (a hole or cache hit on a
settled image, which is what a booting guest reads) was queued without any
signal, so it waited for an unrelated wake. The native harness and the
loopback tests poll `receive()` and never saw it; this is the first time a
real guest booted on a `--fast-settled-reads` host. Regression test:
an inline hole read makes the Local's completion eventfd readable before
`receive()` is called.

**Second attempt (`ba649d9`):** flags off N=2 passed all six cases; both
flags still stalled the first boot. With gdb attached to the stalled host
(`results/integration-1113/gdb-*.txt`): `vring_worker` blocked in
`state::Gate::lock` called from `fast::Context::enqueue`, the main thread
blocked in `HostGate::failure` behind it, reactor and transport idle. The
vhost-user backend holds the image completion gate (host failure mutex plus
image state) across admission and publication, and the fast path, running on
that thread, locked the same gate to check for a failure and to record one:
a self-deadlock on the first settled read of a booting guest; the service
loop then blocked behind it (telemetry stopped after seven seconds) and the
guest hung in its disk probe. The native harness never holds the gate around
`enqueue`. Fix (`c571079`): `Gate::is_failed` (an atomic mirror written when
a guard drops) and `Gate::fail_pending` / `HostGate::fail_pending` (applied
by the next `lock()` / `failure()`); the fast path uses only these, and
`receive(wait)` bounds its eventfd wait as a safety net because the
backend's epoll handler drains the same descriptor. Regression: an inline
read with the gate held is served; a failure recorded under the gate
declines the next inline read without locking.

**Third attempt (`97575b8`, after both fast-path fixes): every suite
passed.** N=2 flags off: `flows-k1`, `flows-k2`, `reads-memory`,
`reads-storage`, `raw-rsync`, `independent-stores` (6/6, 10:49–10:56 UTC);
N=2 both flags (`--peer-race-us 100 --fast-settled-reads` on every fleet
host): 6/6 (10:56–11:03); N=4 flags off: `flows-k1`, `flows-k2`,
`reads-memory`, `reads-storage`, `independent-stores` (5/5, 11:03–11:08);
N=4 both flags: 5/5 (11:08–11:14); the fast-only `flows-k1` attribution run
passed as well. All members on one 8-vCPU VM (placement and protocol across
one kernel, not host isolation, as the 2026-11-09 N=4 run); the earlier
`reads-storage` writer timeouts of the first attempt did not recur and are
recorded as unexplained one-offs of that attempt. Controller and host
outputs are in the evidence archive (`mh/<run>/`).

Evidence: `results/integration-1113/{skiploop,lockloop,lockloop2,loop3,campaign2,campaign3,multihost}.log`,
`evidence/evidence-2.tgz` (every VM log, loopback JSON, controller and host
outputs of the multi-host runs) with its SHA-256 in `evidence/export-2.log`;
`multihost/*.json` are the configurations. VM ledger `resources.json`
(`vm-0b06f9f7927c4a8093127b61daa71248` created, granted, deleted).

## 2026-11-13 — Measurement flags: attach window, metadata budget, per-image reads, shipping window

Branch `feat/measure-flags` from `main` (`35add83`, code identical to the
measured revision `ebbe157`). Four commits, each with tests and documentation:
`7d7e2db` `--attach-timeout-secs`, `f0bbc1d` `--metadata-bytes`, `bc8aeb7`
per-image remote-read attribution, `efa269d` shipping durability window.
**Tested revision: `efa269d`**, clean checkout on the VM. The read path
(`local/reactor/read.rs`) is untouched; the sibling `feat/raced-read` branch
owns it. No flag changes a default: without flags the host keeps the 60 s
attach window and the 128 MiB metadata budget, and the new report fields are
additive (`peer.images`, `fleet.shipping`). Nothing was pushed or merged.

### Host and commands

No build or test ran on Spark (editing, Git bundles and provider calls only).
Freestyle x86_64 VM `cas-build-flags` (`vm-e9edd5c6254c486c9d8f7aea18911e79`,
snapshot `freestyle/ubuntu-lg`, Ubuntu 24.04.5, Linux 6.1.102, 8 vCPU, 16 GiB,
64 GiB disk), created for this task with its own identity grant and deleted
after export. Nix 2.35.6 (upstream multi-user installer, flakes enabled). The
branch was shipped as a git bundle; `ulimit -n 65536`,
`CARGO_TARGET_DIR=/root/target`, `CARGO_BUILD_JOBS=8`.

```sh
nix develop -c just check
nix develop -c cargo test -p cas-daemon --locked --lib -- \
  local::host::tests::recovery::service local::host::tests::peer \
  local::host::tests::fleet peer::tests
CAS_FLEET_EVIDENCE=/root/evidence nix develop -c cargo test -p cas-daemon \
  --locked --lib -- shipping_window_ring
```

### Outcomes on `efa269d`

- `just check`: **pass** — formatting, strict Clippy, **577 Rust tests passed,
  0 failed, 30 ignored** (572/30 on the frozen revision; the five new tests
  are listed below), 13 Python tests, `git diff --check`.
- Peer/fleet suites (`local::host::tests::peer`, `local::host::tests::fleet`
  including the two- and four-process fixtures, `peer::tests`): 27 passed,
  3 ignored subprocess entry points; `two_daemon_loopback` and
  `descriptor_limit` subprocess runs passed.
- New tests, all passing:
  - `tests::measurement_flags_keep_their_frozen_defaults_and_reject_zero`
    (`cas-host` CLI): defaults 60 s / 134,217,728 bytes; `0` rejected.
  - `recovery::frontend::cold_attachment_window_expires_with_the_configured_timeout_when_no_guest_connects`:
    a 200 ms window with no frontend fails through the existing
    `recovery deadline expired` path (`connection_ok: false`), the other image
    stays attachable.
  - `recovery::service::metadata_budget_decides_whether_a_large_rebuilt_index_can_be_served`:
    a 4,096-chunk catalog root fails `host_service::serve` under a 256 KiB
    budget with the existing `chunk index metadata budget exhausted` error,
    no file changes, `startup_error` in `host.json`; the same root opens under
    the default budget, its index allocation exceeds 256 KiB, and chunks
    0/2048/4095 read back correctly.
  - `peer::per_image_remote_read_attribution_sums_to_the_host_counters`:
    two images on one host, 5 and 3 remote reads plus one failure after the
    owner stops; per-image counts sum to the host counters.
  - `fleet::process::shipping_window_ring_records_a_delayed_owner_acknowledgement`:
    two processes, owner holding each PUT 300 ms (test-only
    `CAS_FLEET_DELAY=put-before`).

Retained ring from that test (`shipping-window.json`): two batches (63 and 1
chunks, 260,100 and 4,164 PUT body bytes), both `queued_ns = 406,784,488`,
windows 332 ms and 634 ms (the second PUT waits behind the first's delay),
`window_ns_sum = 967,001,619`, `rescan_batches = 0`.

### Failures and retries during development

1. `--metadata-bytes` first used `value_parser!(usize).range(1..)`, which clap
   does not provide; the flag is parsed as `u64` and converted. A test compared
   `usize` chunks with `u64`. Fixed before any test ran.
2. The per-image test expected `chunks = 3` for image 3, but the read path
   records the failed GET's chunk with zero milestones (`recent[].chunks`
   convention), so the first run reported `chunks = 4`,
   `verified_bytes = 16,384`. `verified_bytes` now counts only chunks whose
   `verified_ns` is nonzero (3 × 4,096); `chunks` keeps the ring convention.
   This is the only runtime assertion that failed; documented in
   `peer-fetch.md`.
3. `just check` failed once on `cargo fmt` (two layout diffs); applied, then
   passed in full.

### Design notes and limits

- The attach window still starts with the recovery scan (one deadline created
  beside it), so the default is byte-for-byte the previous behaviour. Retained
  reconnection and the recovery scan keep the fixed `RECOVERY_TIMEOUT`.
- Only `resources.metadata` follows `--metadata-bytes`; the compaction budget,
  cache, staging and `Shared::new` reference backends keep their constants.
- No root with a real >128 MiB index was built: that needs more than about
  1.5 million distinct chunks (6 GiB or more of compacted data). The test
  demonstrates the mechanism at a 256 KiB budget against a 4,096-chunk root.
  Opening a 16 GiB image under `--metadata-bytes 402653184` remains to be
  observed on the study hosts.
- `fleet.shipping` adds a key beside the flow counters; `runner.py` reads
  flows by name and is unaffected (Python tests pass). The owner's PUT reply
  means its `Store::insert` and sync completed, not fleet-class FLUSH.
- Rescan-originated batches (startup inventory, full queue, retry after a
  failed pass) carry `queued_ns = 0` and are excluded from the window sums.

Ignored evidence in the research repository under `results/measure-flags/`:
`test1.log`, `test2.log`, `test3.log`, `test4.log`, `check-full.log`,
`check-full2.log`, `totals.log`, `shipping-window.json`, the VM ledger
`resources.json` and the control scripts.

### Review follow-up (same day)

Code review found nine issues; fixed in three grouped commits on the branch,
**tested revision `39214ec`** (`82944e7` attach window and metadata cap,
`d0c8ed1` per-image verified bytes, `39214ec` shipper queue and clock), on a
fresh Freestyle VM `cas-build-flags` (`vm-e3e10c29fcfe488b9b2633981b01a95a`,
same shape and procedure as above; `/tmp` on ext4), deleted after export.

- Attach window: it now starts when recovery completes in both cold modes
  (before, ordinary cold mode armed it before recovery). Help text and docs
  say so. New test `attach_window_starts_after_recovery_completes` uses a
  test-only `recovery_delay` (400 ms) with a 300 ms window: the service fails
  after at least 700 ms through the unchanged `recovery deadline expired`
  path with no `startup_error`, where the old sequencing failed at attach.
- `--metadata-bytes` is capped at physical memory (`physical_memory_bytes`,
  sysconf); a value above it is `InvalidInput` before any output exists.
  Sizing text corrected: 16 GiB at 4 KiB is 4,194,304 chunks, 168–336 MB.
- Per-image `verified_bytes` is credited once at transport verification to
  the image whose read issued the GET (each image holds a `Router::for_image`
  view of the shared connections; the request slot carries the image index).
  Coalesced waiters count as reads and chunks, not bytes, so the bytes equal
  `4096 × counters.verified` even when reads coalesce. New test
  `coalesced_remote_reads_credit_verified_bytes_to_the_issuing_image_once`
  holds a mock peer's reply until the second image has joined the first's
  fetch (`fetches.counters.joined`): one GET, one verified chunk, bytes only
  on the issuer. The slot limit is checked once in `make_image`;
  `Metrics::bind` is infallible.
- Shipper: the queue holds `(hash, Option<queued_ns>)` pairs, rescans carry
  `None` (`null` in the report), batches copy up to 63 hashes into a fixed
  array for the unchanged `ship()`, and times come from the peer metrics
  clock shared with `peer.recent` (the report's `clock` field says so). The
  two-process test waits on the sender's `fleet.ship.put_acked_chunks`.

Outcomes: `nix develop -c just check` **pass** — **579 Rust tests passed,
0 failed, 30 ignored**, 13 Python tests, strict Clippy, fmt, whitespace.
Targeted suites (`recovery::service`, `peer`, `fleet` incl. subprocess
fixtures, `peer::tests`): 32 passed, 5 ignored entry points. Retained ring
(`shipping-window-review.json`): two batches, windows 337 ms and 640 ms
against the 300 ms owner delay, `window_ns_sum = 977,932,642`,
`rescan_batches = 0`.

Failures and retries: a shadowed `config` binding and a missing SAFETY
comment (`clippy::undocumented_unsafe_blocks`) failed the first compile; one
`cargo fmt` hunk failed the first full check. All fixed and squashed into
their commits. A probe with `TMPDIR=/dev/shm` fails every host test in the
shared fixture at `Store::create_sized` (O_DIRECT on tmpfs), before any test
body, so the new cold-serve test is not ignored: it runs wherever its
siblings run. The XFS-ignored `failed_cold_image_protocol_leaves_the_other_socket_usable`
passed on ext4; `cold_host_supervises_both...` still needs XFS
(`Snapshot::create`). Evidence: `review1.log`, `review2.log`,
`review-check.log`, `review-check2.log`, `probe-cold.log`.

## 2026-11-13 — Importer review fixes (`feat/offline-import`)

Follow-up to the entry below on the same branch. **Tested revision:
`1e4efee104f0ce4a04e2c41adbf6413e90be667d`**, clean; the intermediate commit
`596084e` was format- and Clippy-checked on its own. This record is a later
documentation-only commit. Nothing was pushed or merged. Same host, checkout,
target directory and limits as below (`cas-build`, `/root/cas-import`, ext4
root, `ulimit -n 65536`); Spark edited only.

Changes under review: platform-free `cas_core::import` defaults so `casctl`
builds off Linux; `cas_core::id` (`parse`, `hex`) shared by `cas-host`,
`cas-fleet` and `casctl`; `cas_core::layout` holds the store/image creation
recipe used by both `cas-host init` and the importer; `Store::exists`
replaces the hard-coded geometry marker; a new root must be an existing empty
directory, its segment size must satisfy the host's default staging limits
including the 60% rollover rule, and the compaction interval is bounded by
worst-case preallocated WAL bytes before any output; the catalog is created
before the geometry marker; interrupted `images/<id>` output is renamed to an
unreferenced image name (fleet-importer pattern) so retries proceed and
tickets stay observed; the trailing settle runs only with staged bytes and
emits its `Settled` point; input identity rechecks device, inode, mtime and
ctime; progress prints by last GiB. The default path's WAL/compaction logic
is unchanged.

Outcomes at `1e4efee`: `just check` rc=0 (1m19s); `tests/import.rs` and
`tests/import_crash.rs` passed for both arms with the same chunk counts as
below (2,697/4,192 and 694/1,079; crash cuts 272/1,497 and 75/412 retained
chunks), now also refusing a missing root, 192 MiB segments, a 256 MiB
interval and a non-empty geometry-less directory without writing output, and
retrying after a cut without manual cleanup (two image directories remain,
ticket high-water increases). The 1 GiB benchmark re-ran at `d926e77`, whose
`crates/cas/core/src` tree is identical to `1e4efee` (only a test assertion
differs): random 4 KiB 56.0 MiB/s (262,144 chunks), half 101.5 (129,664),
dedup 87.2 (0); 16 KiB 91.1 (65,536), 131.9 (32,768), 137.7 (0); every
receipt kept the earlier digests, chunk counts, encoded bytes, index bytes
(25,694,192 / 6,426,608), manifest bytes and D == E == P, with
`settle_passes` 16 instead of 17 as intended.

Failures and retries: the first review run failed because the tests still
expected an implicitly created root; the second because the interval check
ran after root creation (fixed by validating before output); the first
intermediate commit failed `cargo fmt --check` on an import ordering and was
recreated; a refusal test asserted `NotFound` where the importer now returns
its own message. Evidence: `results/offline-import/check-final-review.log`
(final check and tests), `check-bench-final.log` (intermediate check,
`d926e77` check attempt, receipts), and `/root/check-final3.log`,
`/root/import-bench` on `cas-build`.

## 2026-11-13 — Review fixes for raced fetch and inline settled reads

Code review of `feat/raced-read` (`f5e693b`) reported 13 findings. They are
fixed on the same branch in five commits grouped by kind, each re-verified on a
fresh Freestyle VM `vm-109d634404a646c5b80ac443c8d797fd` (`cas-build-readpath`,
`freestyle/ubuntu-lg`, 8 vCPU, 16 GiB, Determinate Nix, `nix develop` Rust
1.98.1, `ulimit -n 65536`), then deleted. Edited on Spark; nothing was built or
measured there. Commands as in the 2026-11-12 entry.

| Commit | Findings | Change |
| --- | --- | --- |
| `594f308` peer | 1, 2, 5-test | The armed race lives on the primary slot (`PendingRace` with the challenger's transport handle). It starts at the budget (racer), at once for budget 0, or immediately when the primary fails, from the transport's completion path under the pool lock. A challenger that already finished when attached re-signals the reader's notify. |
| `1fd362e` local | 3, 4, 6 | FLUSH barrier tracked by sent request ids; a FLUSH rejected before it was sent cannot cancel one in flight. Inline reads decline while the completion gate holds a failure. Holes are zeroed per block; no pre-fill. |
| `9e87915` local | 5 | `local/chunk.rs` owns the per-block resolution and the cache-fill rules; the reactor read and the inline read both use it. Inline remote chunks join the shared fetch registry (leader fetches and publishes; waiter is woken through the frontend eventfd via `Registry::lookup_with`); a held entry is never declined. |
| `7a48c4b` peer | 7, 8, 9, 10 | `peer/worker.rs` stop/join protocol for the GET client, racer and local source. The local source blocks on a scheduler-bound grant eventfd. `race_dropped` counter and early return while stopping. One budget representation. |
| `87a6f49` local, peer | 11, 12 | `Io` built once; inline path takes and returns it. Per-Server test reply delay carried by `Resources` (cfg(test)) replaces the process-global static. |

### Outcomes

Per commit, standalone on the VM (strict Clippy; `cargo test -p cas-daemon
--lib peer` and `... fast`, `RUST_TEST_THREADS=4`), on the content of each
commit before the last two test-only assertion changes were folded in (the
code is identical): `594f308` Clippy clean, 26 peer tests passed, 1 fast test;
`1fd362e` clean, 26 peer, 2 fast; `9e87915` clean, 27 peer (the registry
coalescing test joins here), 3 fast; `7a48c4b` clean, 27 peer, 3 fast;
`87a6f49` clean, 27 peer, 3 fast. Final tree (`92d0019`, same code): `just
check` passed (fmt, strict Clippy, 583 Rust tests passed, 30 ignored, Python
tests, diff check); fleet process suite passed with `CAS_PEER_RACE_US=50`,
with `CAS_FAST_SETTLED_READS=1` and with both (19 passed, 1 ignored entry
point, each); peer suite passed with no flags, fast, race and both (27 each);
three release loopback runs per arm and both delayed-server runs passed. After
the final test-only folds (`87a6f49`): strict Clippy clean, peer suite 27/27 in
all four arms, fast tests 3/3, release delayed-server test passed. New
coverage: primary failure before the budget (refused connection and EOF after
the request) completes from the local copy and stays closed without one; the
two-process fail phase asserts the post-loss read completes locally; a held
FLUSH keeps declining the bypass while a rejected FLUSH completes; a failed
gate declines; two inline reads of one chunk issue one GET through the
registry (`shared_waits` 1, `fetches.joined` 1).

Three test assertions were timing-dependent under the parallel debug suite and
were changed to counter checks with hang guards: the delayed-peer wall-clock
bounds (40 ms for 8 reads; 700 ms for 700 reads) and an immediate
`race_discarded == 1` after a local win (the losing reply's discard lands when
its transport finishes). Each failed once and passed in every other run; the
counters they now rely on were asserted in the same tests all along.

### Loopback re-measurement with the shared resolution

Same harness and method as the 2026-11-12 entry (release, QD1, 4 KiB, 64
chunks, 100 warmup + 600 measured, three runs per arm, nearest-rank quantiles
from the ring, microseconds). Different VM instance from the previous entry;
compare arms within this table, not across entries.

| Arm | Receipt→send p50 | Receipt→send p99 | Verified→completion p50 | Verified→completion p99 | Total p50 | Total p99 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Flags off | 25.9–29.3 | 42.7–73.6 | 32.3–40.8 | 49.7–96.9 | 93.4–118.3 | 137.5–284.3 |
| `--fast-settled-reads` | 7.9–12.2 | 17.9–26.0 | 14.8–19.6 | 34.6–43.4 | 80.0–81.7 | 134.1–155.6 |
| Race 100 µs + local, fast off | 22.3–29.8 | 36.5–58.1 | 31.6–41.5 | 56.0–70.6 | 101.6–126.8 | 138.0–197.3 |
| Race 100 µs + local + fast | 8.0–8.8 | 14.8–19.9 | 14.2–21.2 | 30.3–36.9 | 70.9–86.7 | 113.5–132.1 |

Measured on tree `92d0019`, equal to the final commits except a test-only
assertion change. The shared resolution did not change the shape of the
result: the fast path still removes both reactor wakes (receipt→send ≈8–12 µs,
verified→completion ≈15–20 µs) and the total median falls from roughly
93–118 µs to 80–82 µs (both flags 71–87 µs); the off arm's 284 µs p99 is one
run's tail on this VM. Fast-arm counters per run: `remote_reads` 698,
`declined_io` 2 (the two manifest page loads), `shared_waits` 0 at QD1,
`declined_unsettled` 0. The race arm started 11–64 races per 700 reads (3–11
local wins), every start had one discard, `race_dropped` 0. Delayed server
(1 ms reply, 100 µs budget): 700/700 local wins with and without the fast
flag, `race_discarded` 700, `race_wasted_wire_bytes` 2,884,000,
`race_dropped` 0, total median 462.0 µs (fast off) / 417.4 µs (fast on): the
local source still costs ≈240–280 µs before its load starts (budget plus racer
and worker hops, now a blocking grant wait instead of 20 µs sleeps) and
≈130–180 µs for the scheduled load.

### Findings I read differently

- Finding 3: the `paused` path cannot reject a FLUSH while one is in flight,
  because `Local::pause` is a barrier that waits for the reactor to drain.
  The rejected-before-sent completions that can coexist with an in-flight FLUSH
  come from `send` or `seal` failures, which the id-tracked barrier now covers;
  the test forces the reject through the `paused` flag because that is the only
  deterministic way to produce one.
- Finding 8: the GET service (`peer/server.rs`) also polls `take()` with
  100 µs sleeps; it does not block on a signal. The local race source now
  binds a grant eventfd to its scheduler port and blocks on it; the server's
  loop is unchanged and remains a separate cleanup.
- Finding 2 changes the force-remote experiment's failure semantics with
  `--peer-race-local`: a read after peer loss now completes locally whenever a
  verified local plan exists, instead of failing closed. The documented
  behaviour and the two-process fail phase assert that deterministically.

### Failures, retries and fixes during this round

- The first split of the correctness commits was wrong (a soft reset kept
  every change staged, so the peer commit absorbed the local changes); the
  commits were rebuilt by file set and the final tree checked to be identical.
- Clippy under `cfg(test)`: `too_many_arguments` on the GET server once the
  per-Server delay was a parameter (folded into `ServiceOptions`); a missing
  `SAFETY` comment; an unused parameter in non-test builds; two borrow errors
  in the reactor refactor; an unused import; a dead accessor.
- Barrier test: the control permit must be taken before setting `paused`.
- Coalescing test: an undelayed loopback owner could answer before the
  second read joined; the owner is now a by-hash mock answering after 20 ms.
- `force_remote_race_local_wins...` bounded eight reads at 40 ms of wall time
  and failed once under the parallel suite (143 ms, before its counter checks
  ran); the bound is now a hang guard and the counters carry the assertion.
- A per-commit verification run was started on a mid-rebase tree and
  discarded; the rebase conflict was resolved and the run repeated.
- The fresh VM's first SSH attempt used the previous identity's token; the
  helper now uses the identity granted on the VM.

Evidence: `results/readpath/review/` (per-arm `reader-*.json`, delayed-server
JSON, logs summarized here), `ledger.json` (both VM ids, identities, deletion).
No CI, push, merge or deployment occurred.

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

## 2026-11-13 — Campaign follow-ups: N-guest native runner, chroot working set, nvme-tcp floor, repeats

Branch `feat/cloudlab-campaign`. **Tested revisions `d954ff7`** (native
runner, two-host chroot form, r0.sh nvme-tcp, campaign changes; rehearsal
003), **`4cae14c`** (the chroot wrapper's `command -v chroot` fix and the
driver's native-ZFS storage selection) **and `c0f7df4`** (the replay result
records the working-set spec; the passing chroot run and the final dry
run), all clean on the VM checkout; this record is a later commit. Changes
in `crates/harnesses/src/native.rs` and `native/runtime.rs` (harness only,
no daemon code), `experiments/native/workload.sh`, `experiments/two-host/
{runner,agent,test_runner}.py`, `experiments/cloudlab/` and docs. Nothing
pushed or merged. Same hosts as the entry above: Spark for edits, bundles,
Python unit tests and `cargo check`; every build, Rust test and run on the
persistent Freestyle VM `cas-build` (loop-backed XFS, nested KVM). No VM
created or deleted; the loop images were unmounted afterwards.

### Commands (cas-build, `/root/campaign/driver3.sh`, `rerun-chroot2.sh`)

```sh
bash $SRC/experiments/cloudlab/setup.sh --bundle campaign4.bundle --revision $REV --source $SRC \
  --skip-disk --skip-irq --skip-nix --skip-smt --cores 6 --out out/node-inventory-setup-3.json
nix develop -c cargo test --locked -p cas-harness native
nix develop -c uv run --no-project python -m unittest discover -s experiments/two-host   # 36
nix develop -c uv run --no-project python -m unittest discover -s experiments/cloudlab   # 28
taskset -c 0-7 $SRC/result-native/bin/cas-native-run --backend raw --guests 2 --device $LOOPA \
  --storage /mnt/fleet-a/native-2g-baseline --output native-2g-baseline \
  --script $SRC/experiments/native/workload.sh --script-arg baseline --script-arg 5 --script-arg 64 --allow-loop-device
# the same with --script-arg pressure
$SRC/result-two-host/bin/cas-two-host --config ws-chroot-001-config.json --output ws-chroot-001/controller
bash $SRC/experiments/cloudlab/r0.sh --nvme-tcp-check
python3 campaign.py dry-run --nodes nodes-dry3.json --plan plans/dev-loopback.json
python3 campaign.py run --nodes nodes-r03.json --plan plan-r0.json --export /root/campaign/export   # r0_nvme_tcp false
python3 campaign.py repeats --nodes nodes-r03.json --out out/dev-r0-003/day4-repeats.json
```

### Outcomes

- `cargo test -p cas-harness native`: **5 pass** (guest bound 1..16, raw-only
  ZFS, pool-membership parsing, the two existing refusals). Python: 36 and
  28 pass on the VM and on Spark. `nix build .#native` and `.#two-host` at
  both revisions.
- `cas-native-run --backend raw --guests 2`, `baseline 5 64` and `pressure 5
  64` on the loop-backed XFS: both `outcome.json` `success: true` (111 s and
  102 s). Per-guest fio JSON under `guest-1/`, `guest-2/`; `aggregate-<label>
  .json` per label. Baseline `read-q1`: 2 guests, 16,560 IOPS summed, per-
  guest clat p50/p99/p99.9 max 80/175/289 µs; `write-q1` 12,912 IOPS;
  `flush` 3,343 IOPS; errors 0. Pressure: `reader-alone` 1 reader 8,636
  IOPS p99 167 µs; `mixed-8m-reader` 8,739 IOPS p99 159 µs; `mixed-0m-reader`
  (uncapped writer) 8,043 IOPS p99 220 µs. `identity.json` records
  `guests: 2`, `storage_fstype: xfs`, `native_media: false`. Development
  numbers on a shared VM's virtual disk.
- `ws-chroot-001` (`c0f7df4`, two-host `resume-elsewhere` on the loopback
  layout, 128 MiB image, 2 MiB setup): **`success: true`** (58 s). The
  resume `setup_script` built a synthetic root at `/mnt/cas/root` in the
  source guest (the NixOS guest's bash plus the `/nix/store` libraries that
  `LD_TRACE_LOADED_OBJECTS` lists); after migration B replayed three
  bash-builtin commands as `env -i GREETING=... PATH=/bin "$CHROOT"
  /mnt/cas/root /bin/bash -lc 'cd /work && ...'`. Retained stdout:
  `hello-from-chroot /work` (the recorded env and cwd inside the chroot),
  `bin/ etc/ nix/ tmp/ work/` and `5.3.9(1)-release` (the chroot's own
  bash). Cold pass 443 ms with 646 destination GETs (the chroot's bash and
  libraries fetched from A), warm pass 262 ms with 0 GETs; time to working
  set 20.95 s of which boot 18.21 s; downtime 21.58 s. `results.json`
  `working_set` records `chroot`, `guest_cwd` (`/work`), `env` and the
  exact per-command script. Development evidence (nested KVM, loop files).
- `r0.sh --nvme-tcp-check` on cas-build: modules `nvmet`, `nvmet-tcp`,
  `nvme-tcp`, `nvme-fabrics` missing, `nvme` CLI missing, configfs missing,
  root ok; exit 1. The Freestyle kernel has no module tree, so the nvme-tcp
  phase could not run there; on Spark (NixOS) the same check finds the four
  modules and reports only `nvme-cli` and `root` missing. The phase is
  therefore shipped with its self-check only; its configfs sequence follows
  `docs/review/transport.md` and has not been executed.
- `campaign.py dry-run` (`c0f7df4`, `dev-loopback.json`, nodes file with
  the placeholder `expected_revision`, `r0_nvme_tcp` default): **3 runnable,
  9 blocked of 12**, exit 2, nothing launched. New blocks since the previous
  entry: `r0 floor` ("nvme-tcp prerequisites missing: module-nvmet=missing;
  ...") and `native-zfs g1` ("node 0 has no zfs_storage_mount/zfs_device
  for the ZFS arm"); the other seven as before (runner without the flag
  settings, guest-cpu budget). The loopback layout bypasses the SMT and
  revision-pin rules, which are unit-tested.
- Campaign `dev-r0-003` (`r0_nvme_tcp: false`): R0 unit done (99 s), export
  and end inventories as before. `campaign.py repeats`: "0 unit(s) to
  repeat; keep d1 r0 floor spread=0.0 measured=1/1", plan written with an
  empty day list.
- `setup.sh` at `d954ff7` with `--skip-smt` (the persistent VM's grub is not
  ours to change; the step's runtime path was not exercised): all other
  steps as before; inventory `skipped` now lists `smt`.

### Failures and retries

- Rehearsal 003's first launch ran the previous revision's `setup.sh`
  (the driver lacked the bundle checkout before `setup.sh`), which rejected
  `--skip-smt` (usage, exit 2); relaunched with a bootstrap step.
- `ws-chroot-001` attempt 1: the runner rejected the config ("burst regions
  must fit the image"): the 128 MiB test image needs an explicit small burst
  block even for a resume-only case. Attempt 2 (`d954ff7`): the setup script
  built the chroot (bash plus its `/nix/store` libraries) but the replay
  failed with `env: 'chroot': No such file or directory`: `env -i` resolves
  its program in the *new* environment (`PATH=/bin`), where the NixOS guest
  has no `chroot`. Fixed in `4cae14c` (`CHROOT=$(command -v chroot)` on the
  guest, then `env -i ... "$CHROOT"`), with a unit test that runs the wrapper
  through `sh -eu`. Attempt 3 (`4cae14c`): all three commands ran correctly
  in the chroot (retained stdout) but the runner crashed after the warm pass
  with `NameError: name 'commands' is not defined` in `replay_working_set`'s
  return value, which the unit tests had not covered. Fixed in `c0f7df4`
  with a test that drives `replay_working_set` against fake agents.
  Attempts 2 and 3 are retained as `ws-chroot-001-attempt-{2,3}`.
- The bridge's exec channel times out when a command backgrounds a process
  inside `( ... &)`; the processes still start. Launches now use `setsid
  nohup` and are confirmed by a second command.

### Evidence

`results/cloudlab-campaign/evidence/campaign-rehearsal-003.tgz` with
`SHA256SUMS` (driver and step logs, `out/dev-dry-003`, `out/dev-r0-003` with
`day4-repeats.json`, the setup inventory, `native-2g-baseline`,
`native-2g-pressure`, `ws-chroot-001` and its attempt 2, the configs), plus
`driver3.sh`, `rerun-chroot.sh`, `rerun-chroot2.sh` and the nodes files.

### Conclusions and limits

The native runner boots N guests with their own disks and jobs and writes
per-guest and aggregate results (N=2 exercised; 16 is a bound, not a tested
load); the ZFS acceptance for the raw backend is unit-tested only, no ZFS
pool exists on the VM. The two-host chroot working set replays a recorded cwd and environment
inside the data disk's root, shown with a synthetic bash-only root; a real
Stage A root image has not been replayed.
The nvme-tcp floor row is unexecuted code with a self-check. The repeats
generator ran on one unit; its p99 rules per case are unit-tested on
synthetic results. Everything is development evidence on one shared VM.

## 2026-11-13 — CloudLab campaign driver rehearsal on cas-build

Branch `feat/cloudlab-campaign` from `feat/burst-harness` (`5a69820`).
**Tested revisions `5633fb9` (rehearsal 001) and `4e0e65e` (rehearsal 002)**,
both clean on the VM checkout; this record and two small fixes (`r0.sh`
fdatasync sample count, `inventory.py` NIC summary; the exact files were
verified standalone on the VM, see below) are a later commit. Only
`experiments/cloudlab/` and `docs/` changed; nothing under `crates/`, so
the daemon is still the `ebbe157` build (`cas-host`
`cc1c4ae4…`, `cas-fleet` `8ccdb986…`). Nothing pushed or merged.

Spark handled edits, git bundles, unit tests and evidence; every build and
run used the persistent Freestyle VM `cas-build`
(`vm-7ecd8e364c624bcb8654fa4dbd92767e`, x86_64, 8 vCPU, 16 GiB, Linux
6.1.102, nested KVM) through the sandbox-corpus `control.py` bridge (scoped
SSH; files over SFTP). A new clone `/root/cas-campaign` was made from the
bundle. No VM was created, resized or deleted.

### Commands

```sh
# Spark
uv run --no-project python -m unittest discover -s experiments/cloudlab      # 22 pass
# cas-build, /root/campaign/driver.sh then driver2.sh (loopback layout: both
# members on the VM, loop-backed XFS at /mnt/fleet-a and /mnt/fleet-b, daemons
# 0-1 / 2-3, guests 4-7, nodes-*.json with "loopback": true)
bash $SRC/experiments/cloudlab/setup.sh --bundle campaign.bundle --revision $REV \
  --source /root/cas-campaign --skip-disk --skip-irq --skip-nix --cores 6 --out out/node-inventory-setup.json
python3 $SRC/experiments/cloudlab/campaign.py dry-run --nodes nodes-dry.json --plan $SRC/experiments/cloudlab/plans/dev-loopback.json
python3 $SRC/experiments/cloudlab/campaign.py start   --nodes nodes-r0.json --plan plan-r0.json --export /root/campaign/export
python3 $SRC/experiments/cloudlab/campaign.py status  --nodes nodes-r0.json
python3 $SRC/experiments/cloudlab/campaign.py run     --nodes nodes-r0.json --plan plan-r0.json --export /root/campaign/export [--redo]
```

### Outcomes

- Unit tests: **22 pass** on Spark (plan loader shapes/rejections/rotation,
  arm validation of every checked-in config against its own pinning sets,
  `check_pinning` rules, placeholders, the race-budget rule, bundle working
  set, unit states, `decide`, the driver's skip/refuse/redo path through
  `Campaign.step`, wrapper parsing, runner-support detection, node
  validation, inventory NIC parsing).
- `setup.sh` without disk work (`--skip-disk --skip-irq --skip-nix`): all
  steps exit 0 in 43 s (bootstrap clone, `checkout` at the pinned SHA,
  `nix build .#two-host` and `.#native` from cache, `limits`, `inventory`);
  `build.json` records both wrappers' build-info paths and binary hashes;
  `node-inventory.json` lists `skipped: ["disk", "build", "irq"]` for the
  inventory-only snapshots. 69 hardware records, 22 recorded as unavailable
  on the VM (no `nvme`, `dmidecode`, ZFS, cpufreq).
- `campaign.py dry-run` with `plans/dev-loopback.json`: **4 runnable, 8
  blocked of 12 units**, exit 2, nothing launched. Blocked for the intended
  reasons: `fast`, `raced`, `resume` units because the built runner at this
  revision does not know `settings.fast_settled_reads` ("the arm would be
  mislabelled"); `fast reads-storage-g8`, `native-cas g4` and `burst
  burst-images` because 8/8/16 guest cpus exceed the loopback set of 4;
  `native-zfs g1` because `cas-native-run` requires XFS. Manifest bound to
  revision and binary hashes, identical on both members.
- Campaign `dev-r0-002` (revision `4e0e65e`): `start` detached the worker
  via `systemd-run --unit cas-campaign-dev-r0-002` with PATH/HOME handed
  over; the R0 unit ran end to end (**done, 97.2 s**), `SHA256SUMS` written
  per unit and campaign, rsync export to `/root/campaign/export/dev-r0-002/`,
  start and end inventories for both members. A second `run` kept the unit
  ("kept from an earlier run", nothing launched); `run --redo` moved it to
  `d1-r0-floor-r1.attempt-1` and reran it (done, 98.6 s). `status` prints
  the one-screen table at every stage.
- R0 on the loopback layout (development numbers: loop-file XFS over the
  VM's virtual disk, both members on the same host, Python ping-pong over
  127.0.0.1→127.0.0.2): 4 KiB QD1 direct random read p50 26.8 µs, p99
  82–92 µs, ~31 k IOPS per member; 4 KiB QD1 write p50 32–35 µs; fdatasync
  p50 129.5 µs, p99 485 µs (from the standalone check of the fixed `r0.sh`,
  5 s run); ping-pong RTT p50 44–48 µs, p99 71 µs, 605 k round trips in 30 s,
  tool labelled `python-tcp-pingpong`.

### Failures and retries

- First upload through the bridge's Freestyle `write` API hung for 15
  minutes with nothing created on the VM; stopped and replaced by SFTP over
  the same scoped transport (helper kept with the evidence).
- Campaign `dev-r0-001` (revision `5633fb9`): the `systemd-run` worker had
  no Nix profile in its environment, so `r0.sh --check` found no fio and
  the unit was **blocked** in the detached run; the foreground resume pass
  on the same campaign then ran it successfully (done, 95.6 s). Fixed in
  `4e0e65e` (`--setenv` PATH/HOME, Nix profile lookup without `~`) and
  re-proven by `dev-r0-002`.
- fio 3.41 reports `sync.total_ios = 0` for `--fdatasync=1` jobs and counts
  samples in `sync.lat_ns.N`, so the first R0 summaries had an empty
  `fdatasync` block (the raw `json+` files hold the distribution). Fixed in
  `r0.sh` (`sha256 1528fba4d1fc…`), verified standalone on the VM from
  `/root/campaign/r0-fix/` (26,988 samples). The inventory NIC summary
  matched `ethtool-c-*` records as interfaces; fixed in `inventory.py`
  (`sha256 14e507a1a081…`) and verified against the recorded inventory records.
- `systemctl status` after the run reports the unit as not found: `--collect`
  removes it on exit; it is visible while running.

### Evidence

Retained under ignored `results/cloudlab-campaign/evidence/` with
`SHA256SUMS`, digests matching the VM: `campaign-rehearsal-001.tgz`
(driver log, step logs/statuses, `out/` with both campaigns' directories,
inventories, manifests, status, dry-run report, R0 unit, export tree),
`campaign-rehearsal-002.tgz` (the same for revision `4e0e65e` plus the
resume/redo passes and `attempt-1`), `campaign-rehearsal-002-r0fix.tgz`
(the fixed scripts and their standalone outputs). Loop images were
unmounted; `/root/campaign` and `/root/cas-campaign` remain on the VM.

### Conclusions and limits

The driver's plan loading, validation, pinning checks, manifest binding,
detached execution, resume/refuse/redo, per-unit export with hashes and
status display work on a loopback development layout. No guest was
launched: the two-host and native arms were validated by the dry run only,
and at this revision every flagged arm is correctly blocked until the
integration branch's runner is the pinned build. R0 numbers are development
evidence (virtual disk, loopback, Python loop), not a testbed floor. Disk
formatting, IRQ pinning and the Nix install steps were not exercised.

## 2026-11-12 — Offline raw-image importer (`casctl import`)

Branch `feat/offline-import` from `main` (`ebbe157`). **Tested revision:
`0086071dc047a215a2f3af1760eeeeb3e0ec11ea`**, clean on the test checkout. This
record is a later documentation-only commit. Nothing was pushed or merged.
These are Freestyle development checks on an ext4 root filesystem, not CI,
paper, native-device or host-serving results. The daemon and on-disk format
are unchanged; `cas-core` gains `import` and a `sha2` dependency (lockfile
updated), `casctl` gains the `import` verb.

### Hosts and commands

Spark edited sources and read evidence only. All builds, checks, tests and
imports ran as root on the retained Freestyle VM `cas-build`
(`vm-7ecd8e364c624bcb8654fa4dbd92767e`; Linux 6.1.102, 8 vCPU, 16 GiB,
`/dev/vda` ext4 root, rustc 1.98.1) in a separate checkout `/root/cas-import`
with `CARGO_TARGET_DIR=/root/cas-import/target`, `ulimit -n 65536`,
`CARGO_BUILD_JOBS=8`, while another branch used `/root/cas` on the same VM.
Storage tests used the ext4 root for 4 KiB-aligned `O_DIRECT`, as allowed by
`docs/testbed.md`.

```sh
nix develop -c cargo fmt --all            # formatting diff applied before the final commit
nix develop -c just check                 # rc=0, 1m45s wall at 0086071
nix develop -c cargo test -p cas-core --locked --test import --test import_crash -- --nocapture
nix develop -c cargo build --release --locked -p cas-cli
casctl import --root root-ARM --store 0101…01 --image 0a…0a --from random.raw --chunk-bytes ARM
casctl import --root root-ARM --store 0101…01 --image 0b…0b --from half.raw
casctl import --root root-ARM --store 0101…01 --image 0c…0c --from random.raw
cargo run --release --locked -p cas-core --example chunk-arm -- inspect root-ARM
```

### Outcomes

- `just check` at `0086071`: formatting, Clippy (`-D warnings`), every
  workspace test (including the existing chunk crash cuts), the two-host
  Python unit tests and `git diff --check` passed.
- `tests/import.rs` (both arms): two 12 MiB + 4 KiB synthetic images sharing
  their first half, 2 MiB segments and a 2 MiB compaction interval. Unique
  chunks appended equalled the independently computed nonzero-chunk union
  (4 KiB: 2,697 for A, 4,192 for A∪B; 16 KiB: 694 and 1,079, including the
  zero-padded tail unit); ZERO bytes equalled the all-zero units; catalog
  listed both images with the right sizes; a cold reopen through
  `Tickets`/`Store::inspect_recorded`/`Manifest::inspect`/`Log::inspect_shared`
  and `read_plan`/`read_with` returned bytes whose SHA-256 matched the inputs
  and the receipts. Re-importing the same ID, a foreign store ID, an unaligned
  input and the other chunk arm were refused without creating directories.
- `tests/import_crash.rs` (both arms): a child process exited at the
  `Settled` (after the first 2 MiB pass), `Durable` (D == E == P before
  catalog publication) and `Published` points. After each cut the store and
  catalog inspected and recovered; the catalog listed the image only after
  `Published`; interrupted imports kept their `images/<id>` directory, a
  retry was refused until it was removed (behaviour changed by the 2026-11-13
  entry), then deduplicated against the retained chunks (4 KiB: 272 of 1,497
  after `Settled`), and every arm/cut read back exactly.
- 1 GiB release-build imports at `0086071` (receipts in the evidence log),
  single process on the VM's ext4 root, 64 MiB segments and interval:

| Input | Arm | Unique chunks appended | Elapsed | MiB/s |
|---|---|---|---|---|
| random 1 GiB, new root | 4 KiB | 262,144 | 18.7 s | 54.9 |
| half zero (512 MiB random, 4 MiB alternation, every 97th block zeroed) | 4 KiB | 129,664 | 10.8 s | 94.9 |
| random 1 GiB again (full dedup) | 4 KiB | 0 | 11.4 s | 89.9 |
| random 1 GiB, new root | 16 KiB | 65,536 | 11.1 s | 92.3 |
| half zero | 16 KiB | 32,768 | 7.7 s | 133.7 |
| random 1 GiB again (full dedup) | 16 KiB | 0 | 8.2 s | 125.3 |

  Every receipt reported D == E == P equal to its mutation count, the
  expected SHA-256 (`a66ca740…` random, `8464a681…` half), 17 settle passes,
  16 (or 7–8) removed WAL segments and the WAL punched back to one retained
  segment. The 4 KiB root ended with 391,808 chunks, 1,636,405,248 encoded
  bytes and a 25,694,192-byte rebuilt index (65.6 bytes per chunk); the
  16 KiB root with 98,304 chunks, 1,623,298,048 encoded bytes and 6,426,608
  index bytes. `chunk-arm inspect` on both roots reproduced the receipts'
  chunk counts and index memory exactly. Manifest output was 50.5 MB per
  1 GiB random image at 4 KiB and 24.7 MB at 16 KiB (one COW transaction per
  1 MiB batch). The first run of the same benchmark at WIP `1a668d0` (same
  staging logic before the refactor) gave 51–159 MiB/s with identical chunk
  counts and digests.

### Failures and retries

A borrow of a temporary in `tests/import.rs`, an odd-block half-image
assertion that printed 12 MiB vectors, and Clippy's `manual_is_multiple_of`
on a test predicate each failed one build and were fixed; the first complete
`just check` then passed on the fmt-applied tree and again on the committed
revision. The 1 GiB benchmark ran twice (WIP and final revision); the
offline inspect step was rerun from the checkout after a wrong working
directory. No VM was created or deleted.

### Evidence and limits

Ignored `results/offline-import/` holds the check logs
(`check-final-0086071.log`, `check2-refactored.log`, `check1-bench.log`),
first test runs, both benchmark logs with full receipts
(`bench-1gib-final-0086071.log`, `bench-1gib-receipts.txt`); the same files
and the roots remain under `/root/import-bench` and `/root/check*.log` on
`cas-build`. The importer is single-threaded and shares the host compactor's
per-batch chunk syncs and per-transaction manifest syncs; no `--direct`
store path was added. Throughput is one run per case on a shared VM disk
with the page cache warm from input generation. A 10–16 GiB image at 4 KiB
would need roughly 170–270 MB of rebuilt index at the measured 65.6 bytes
per chunk, above `cas-host`'s fixed 128 MiB metadata budget; serving such a
root is not established here.

## 2026-11-12 — Raced fetch and frontend-inline settled reads

Branch `feat/raced-read` from `ebbe157`: `d6f7d4b` (peer: race a settled GET
against a second owner or the local copy) and `c225e73` (local: frontend-inline
settled reads behind `--fast-settled-reads`), both committed with their tests;
neither pushed nor merged. Edited on Spark; every build, test and measurement
ran on a dedicated Freestyle VM `vm-38331efffa414b098f8136cacf1e903c`
(`cas-build-readpath`, snapshot `freestyle/ubuntu-lg`, 8 vCPU, 16 GiB, x86_64,
Ubuntu, Determinate Nix 3.24.2 / nix 2.35.3, `nix develop` toolchain Rust
1.98.1, `ulimit -n 65536`, `CARGO_TARGET_DIR=/root/target`). No other agent
used the VM; it was deleted after this record was written (see below). Commands
from the checkout root, inside `nix develop -c env CARGO_BUILD_JOBS=8 ...`:

```sh
just check                                   # RUST_TEST_THREADS=4
CAS_PEER_EVIDENCE=... cargo test --release -p cas-daemon --lib --locked \
  local::host::tests::peer::two_daemon_loopback_verifies -- --nocapture
CAS_PEER_RACE_US=100 CAS_PEER_RACE_LOCAL=1 ...   # race arm, reader child
CAS_FAST_SETTLED_READS=1 ...                     # fast arm, both children
cargo test -p cas-daemon --lib --locked peer     # with and without the envs
cargo test -p cas-daemon --lib --locked fleet    # CAS_PEER_RACE_US=50, fast, both
```

### Outcomes

Baseline `ebbe157` (clean tree): `just check` passed (fmt, strict Clippy, all
workspace tests, Python tests, diff check); three release loopback runs passed.
Feature 1 at `2e8bccf` (content of `d6f7d4b` minus the two late fixes below):
`just check` passed (580 Rust tests passed, 30 ignored, incl. 25 peer-filter
tests), fleet process suite with `CAS_PEER_RACE_US=50` passed (19 passed, 1
ignored entry point), three flag-off loopback runs passed. Work tree `28b374a`
(content of `c225e73` except the `attach`-after-failure fix, a test settle step
and a test variable rename): strict Clippy clean; `just check` passed (580 passed, 30 ignored); model
test passed; peer suite passed with `CAS_FAST_SETTLED_READS=1`, with both flags
and with neither (25 passed each); fleet suite passed with the fast flag and with
both flags (19 passed each); three loopback runs passed for each of off, fast and
both; the delayed-server race test passed with and without the fast flag.
Final tree `3833175` (equal to `c225e73` except a variable rename in the
loopback test that chooses which report the evidence file holds): the first
commit standalone (`a572253`, equal to `d6f7d4b` minus that rename) was strict
Clippy clean with 25 peer-filter tests passed; the final tree was strict Clippy
clean, `just check` passed (580 passed, 30 ignored), the peer suite passed with
both flags (25), three race-arm and three both-arm loopback runs passed, and the
delayed-server race test passed with and without the fast flag. The rebuilt
commits `d6f7d4b`/`c225e73` were then re-verified (Clippy, peer suite without
and with both flags, one race-arm loopback); see the final line of this entry.

Release loopback, QD1, 4 KiB, 64 chunks, 100 warmup + 600 measured reads,
reader cache fills off, microseconds, nearest-rank quantiles from the ring
records of each run, ranges over three runs per arm. Development evidence on a
shared VM, not a paper result; no CPU pinning or IRQ control.

| Arm | Receipt→send p50 | Receipt→send p99 | Verified→completion p50 | Verified→completion p99 | Total p50 | Total p99 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Baseline `ebbe157` | 21.6–30.1 | 40.8–52.3 | 30.2–36.0 | 49.2–61.2 | 102.5–117.3 | 147.4–181.1 |
| Final code, flags off | 25.5–28.6 | 36.9–58.0 | 31.4–40.9 | 49.1–62.9 | 97.7–121.8 | 130.5–200.9 |
| `--fast-settled-reads` | 6.4–13.5 | 13.4–22.0 | 17.2–22.6 | 26.0–37.8 | 67.4–87.0 | 95.6–122.2 |
| Race 100 µs + local, fast off | 25.1–27.3 | 41.5–254.2 | 33.6–41.2 | 51.8–70.5 | 94.0–120.2 | 139.6–410.0 |
| Race 100 µs + local + fast | 6.6–7.2 | 12.2–16.2 | 14.1–16.0 | 26.5–32.3 | 59.1–75.8 | 88.4–117.0 |

The fast-only and race-only rows come from different trees (`28b374a` and
`3833175`); the both-flags row is from `3833175` (an earlier both-flags set on
`28b374a` measured 56.9–76.0 / 82.3–114.1). The race arm's 410 µs p99 is real:
in that run eight of the 600 measured reads were won by the local copy and each
such win costs ≈400 µs through the thread-based local source, so with an
undelayed peer the race can lengthen the tail rather than shorten it. The
race-arm runs started 21–28 races per 705 reads (9–17 local wins, 4–12 first
owner wins, one discard each) and the post-loss read failed closed in two of
three runs and completed locally in the third, as the test allows.

Where the time went: on this VM the daemon's own legs are thread handoffs.
Receipt→send contains the frontend→reactor eventfd wake and one reactor loop
(≈25 µs median); verified→completion contains the reply→reactor wake through
io_uring's registered eventfd plus the reactor→frontend wake (≈35 µs median);
send→first byte (≈40–50 µs) is B's service path and loopback TCP. The fast path
removes both reactor wakes: receipt→send falls to 6–13 µs (the GET is sent on
the frontend thread) and verified→completion to 17–23 µs (one wake, reply
worker→frontend; the frontend's own copy, cache-fill check and completion are a
few microseconds). Total median improved from roughly 100–120 µs to 67–87 µs;
p99 from 130–200 µs to 96–122 µs. The race alone, with B undelayed, starts for
only the slowest few percent of GETs (6–20 of 700 per run; the 100 µs budget
exceeds the ~45 µs median reply) and does not move the quantiles on its own.

Delayed-server race (B sleeps 1 ms per GET reply, A races the local copy after
100 µs): all 700 measured reads were won by the local copy with and without the
fast flag, every late reply was verified then discarded (`race_discarded` 700,
`race_wasted_wire_bytes` 2,884,000 = 700 × 4,120), `errors` 0, `failed_reads` 0.
Total median 417–469 µs (fast off) / 377–436 µs (fast on): the local source is a
blocking worker reached through the racer thread and the IO scheduler, so a
local win costs ≈240–270 µs before its read starts (100 µs budget plus two
thread hops) and ≈135–160 µs for the scheduled O_DIRECT load. This is the
experiment arm's overhead, not the reactor's io_uring local path.

Race counters from `force_remote_race_local_wins_against_a_delayed_peer...`
(in-process, B delays 5 ms, budget 0): 8 reads, `races_started` 8,
`race_won_local` 8, `race_won_first` 0, `race_discarded` 8, `verified` 8,
`race_wasted_wire_bytes` 32,960; all records `race_winner` 3, `race_budget_us`
0. Corrupting peer with local copy: `errors` 1, `race_won_local` 1, read
succeeds, gate healthy. Both sources corrupt (local payload overwritten on
disk): read fails with IO error, gate failed, `failed_reads` 1, cache empty.
Router-level mocks: second owner wins against a 40 ms first owner (discarded
1, wasted 4,120), a corrupt first reply is masked only by a verified second
(`errors` 1), both corrupt fails (`errors` 2), a 5 ms budget starts no race for
a prompt primary and one for a late primary (`race_budget_us` 5,000).

Fast-path counters (loopback, fast arm): `remote_reads` 698 of 700 per run plus
`declined_io` 2: the first lookup of each manifest page needs a page load and
is declined to the reactor by design (condition 5), `declined_unsettled` 0.
The model test asserts declines for a staged write (`declined_unsettled`), a
pending FLUSH (`declined_flush`), an unsealed overlapping batch, and that the
first settled read of store-resident chunks is declined (`declined_io`) while
the repeated reads are inline.

### Failures, retries and fixes

- Race-arm loopback (`2e8bccf`): my test invariant `discarded + won_first ==
  started` was wrong; every started race has exactly one discard whichever
  side wins. Fixed in the test (`a572253`).
- Race-arm loopback: the pipeline phase counted `verified` before late loser
  replies arrived (702 of 704), and wins can trail starts when the primary
  completes between the challenger's issue and attach; the test now settles
  discards before counting and asserts `wins <= started`, `discarded ==
  started`.
- Race-arm fail phase: after B stops, a refused connection fails the primary
  within the 100 µs budget, so no challenger starts and the read fails closed;
  the test accepts either outcome and checks `failed_reads` accordingly. A
  challenger that was issued but found the primary already failed was dropped
  and the read failed closed although the local copy would have verified;
  `attach` now keeps the challenger for a failed primary (`a572253`), which the
  final race-arm runs below exercised.
- Fast path (`66c44d8` work tree): the reactor published the manifest view only
  on its first loop, so reads enqueued before it were declined (first model
  read; 698 of 700 in loopback) — `Local::start` now publishes it. A challenger
  signalled only the primary slot's eventfd, which the frontend never polls, so
  frontend-issued GETs noticed a local win only when the delayed primary
  arrived (49 ms for 8 reads; 2 local wins of 700) — challengers now signal the
  primary's external notify when present.
- Model test: expected inline service for settled chunks that still require a
  store read; condition 5 declines those and only the repeated (cached) read is
  inline. Test corrected.
- Clippy: unused import and `large_enum_variant` on the decline outcome
  (expected like `backend::admission::Admission`); a borrow in the model test.
- `cargo fmt` diffs were applied locally before each ship.

Retained raw evidence (ignored, local only; the VM was deleted):
`results/readpath/{baseline,f1-off,f2,final}/reader-*.json` (for `final`
race/both runs the file holds the post-loss report because of the rename bug
above; the ring records with ids 100–699 are the measured reads), delayed-server
`reader`/`late` JSON, `summarize.py` (nearest-rank quantiles from the ring),
`ledger.json` (VM/identity ids and events), `final/just-check.log`,
`final/f1-peer.log`, `final/peer-both.log`. VM-side logs under
`/root/results/{baseline,f1,f2,final}` (just-check, fleet, loopback logs and
`revision.txt`) were not exported before deletion except as summarized here.
No CI, push, merge, deployment, CloudLab or guest/QEMU measurement occurred;
loopback timestamps exclude guest scheduling.

### Open for a human

- The local race source is a thread + IO-scheduler demand port (same as GET
  service), not the reactor's io_uring path; its ≈400 µs local win is the
  arm's cost. Moving it onto io_uring would need a second in-kernel owner per
  read.
- The fast path's settled test is image-wide (`published == admitted` and
  `D == P`), not per range; a write-active image falls back to the reactor
  until compaction catches up. A per-region staged bitmap would widen it but
  adds a cas-core invariant.
- Inline GETs bypass the shared fetch registry (duplicate concurrent GETs for
  one hash are possible, never incorrect).
- With `--peer-race-local`, a read can complete from the local copy after peer
  loss instead of failing closed; that changes the force-remote experiment's
  failure semantics and is documented in `docs/peer-fetch.md`.
- `--peer-inline-us` spinning now also happens on the frontend thread for fast
  reads (bounded, same budget); unmeasured here.

Final verification of the rebuilt commits on the same VM: `d6f7d4b` alone was
strict Clippy clean with 25 peer-filter tests passed; `c225e73` was strict
Clippy clean, passed the peer suite without flags and with both flags (25 each)
and one race-arm release loopback whose `reader.json` holds the measure report
(700 GETs, 0 errors). The VM `vm-38331efffa414b098f8136cacf1e903c` was then
deleted through the provider API (`results/readpath/ledger.json` records the
deletion); no other VM was created, touched or deleted.

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
