# Playbook

A prerendered SvelteKit site containing the retained design pages (`00–06`),
meeting deck (`updates/1`), and dated implementation articles (`updates/2–4`).
These pages include proposals and historical measurements; they are not the
current implementation contract. Use [the code documentation](../README.md)
for current behavior and limits.

## Development

```sh
pnpm install --frozen-lockfile
pnpm check
pnpm build
pnpm dev --host 127.0.0.1 --port 43187
```

`pnpm preview --host 127.0.0.1 --port 43187` serves the static build. Keep local
previews on loopback. The production site is intentionally public; historical
research links still require separate access to the private archive.

## Deployment

Production: https://cas-playbook.vercel.app/

The canonical Forgejo `cas` repository mirrors `main` to GitHub. Vercel's GitHub
integration builds that branch for the existing `cas-playbook` project in
`rathiharivansh-gmailcoms-projects`. Other branches do not automatically deploy.
The research repository is not a deployment source.

[`vercel.json`](../vercel.json) is the build configuration: repository root,
Other/static framework, frozen pnpm 11.5.3 install, typecheck and paper generation,
`playbook/build` output, and extensionless HTML URLs. The adapter explicitly
writes `build/` even under `VERCEL=1`; CI checks this output contract. The project
uses Node 24.
Only the generated static directory is served. Configure changes in this file
rather than overriding its build commands in the dashboard.

[GitHub CI](../.github/workflows/playbook.yml) also checks and builds Playbook
on relevant pushes and pull requests. It has read-only repository permissions
and no deployment credentials. Deployment is handled by the Vercel GitHub app,
not a workflow token; a successful GitHub build alone does not prove deployment.
A failed Vercel typecheck/build cannot replace the last successful site.

Every production build includes the typeset paper at `/spec.pdf` and its
`/spec.pdf.sha256` checksum. The install script provisions the PDF tools with
dnf on Vercel's Amazon Linux image and apt on GitHub's Ubuntu runner; Pandoc
3.7.0.2 and uv 0.8.22 downloads are checksum-verified. The build fails if the
PDF is invalid or the header link is missing. The existing Vercel GitHub app
still publishes `main`; no deployment token or Pages workflow is needed.
Local `.vercel/` metadata is ignored, not committed.

## Content and data

`src/routes/00–06` is the retained specification text. Articles use components
under `src/lib/architecture/`; the expandable architecture graph is under
`src/lib/system/`. `src/lib/updates.ts` supplies the meeting deck.

The ten JSON files under `src/lib/` and `src/lib/architecture/` are the
historical data snapshots used by the pages. They are included deliberately
with Playbook; the broader research archive, analysis scripts and raw evidence
are not imported into this repository. Numbers remain bound to their original
revisions and conditions, not to the publication snapshot.

Historical source, PR and evidence links point to private `cas-research` and
require access. They are not publicly reproducible evidence. The current site
source link points to `cas`. Avoid replacing old revision links with current
code: that would misidentify what was measured.

## PDF

```sh
pnpm pdf
uv run --no-project python scripts/pdf/check.py
```

The PDF builder reads the same prerendered numbered pages and writes
`build/spec.pdf`. It needs pandoc, librsvg, latexmk/XeLaTeX, TeX Gyre fonts,
and uv for font conversion; the checker also uses Poppler's `pdfinfo` and
`pdftotext`. The paper icon is always shown beside GitHub. `pnpm build` alone
builds HTML; `pnpm pdf` completes the paper, and production runs both phases
before deployment. Generated HTML, PDFs and dependency directories are build
output, not repository source.

## Publication review

The owner approved publication of the retained proposed designs and displayed
performance results on 19 September 2026. Historical research links remain
private; publishing this site does not publish their targets. First-party site
source is [GPL-3.0-only](../LICENSE). That grant excludes the bundled Berkeley
Mono font; its separate redistribution terms are not recorded here.
Crawling directives are not access control.
