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
