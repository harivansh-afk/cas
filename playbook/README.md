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

Style follows playbook.ix.dev: monochrome stone palette, light-first with an
automatic dark scheme. Type is Berkeley Mono Variable, self-hosted from
`src/lib/assets/fonts/`. Diagrams are inline SVG built from the small component
kit in `src/lib/components/diagram/` (Diagram, Node, Edge, Group, Note,
Bracket); a node's `tone` (accent, outline, muted, ghost) is how a figure says
what matters.

Earlier versions of the spec are in `../docs/history/`; the literature and
design reviews behind the current version are in `../docs/review/`.
