# Validation

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
