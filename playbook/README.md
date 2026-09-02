# playbook

The research spec as a static site: seven numbered pages plus an index, fully
prerendered SvelteKit. The pages under `src/routes/00–06` are the text of
record; `../docs/spec.md` mirrors them in Markdown for review and hand edits,
and changes there are ported back into the pages.

## Develop

```sh
pnpm install
pnpm dev
```

## Build

```sh
pnpm build      # static output in build/
pnpm preview
```

Navigation: arrow keys move between pages, Escape returns to the index.

## PDF

```sh
pnpm pdf        # build/spec.pdf
```

The PDF is typeset from the built pages, so the site is the only source:
`scripts/pdf/build.py` lifts each article out of `build/0N.html`, converts the
inline SVG figures with rsvg-convert, and hands one HTML document to pandoc,
which writes LaTeX (`filter.lua` maps the site's markup, `header.tex` sets the
type). latexmk runs XeLaTeX. Needs pandoc, rsvg-convert, latexmk with XeLaTeX
and TeX Gyre Pagella, and uv (to unpack the woff2 for fontspec). The Pages
workflow runs the same script and deploys the PDF beside the site.

Style follows playbook.ix.dev: monochrome stone palette, light-first with an
automatic dark scheme. Type is Berkeley Mono Variable, self-hosted from
`src/lib/assets/fonts/`. Diagrams are inline SVG built from the small component
kit in `src/lib/components/diagram/` (Diagram, Node, Edge, Group, Note,
Bracket); a node's `tone` (accent, outline, muted, ghost) is how a figure says
what matters.

Earlier versions of the spec are in `../docs/history/`; the literature and
design reviews behind the current version are in `../docs/review/`.
