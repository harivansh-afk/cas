# playbook

The casblk spec as a static site: six numbered pages plus an index, fully
prerendered SvelteKit. `SPEC.md` is the content's source of truth; the pages
render it one page per route.

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
`src/lib/assets/fonts/`. Diagrams are mermaid, rendered client-side and themed
from the same palette.
