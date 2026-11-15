# Paper

Working title: *Images Are Unique, Blocks Are Not: Sharing Disk State Across a
Fleet of Agent Sandboxes*.

| Path | What it is |
|---|---|
| `sections/*.md` | The text, one file per section. Edit these. |
| `paper.tex` | LaTeX transcribed from the sections. |
| `paper.pdf` | Built from `paper.tex` by `build.sh`. Rebuild after editing. |
| `build.sh` | Builds the PDF with TeX Live from the repository's pinned nixpkgs. |
| `refs.bib` | References, each marked opened or not opened. |
| `docs/terminology.md` | The vocabulary, with the source each term follows. |
| `docs/claims.md` | Each claim and what must be verified for it, on CAS and on ZFS. |
| `docs/decisions.md` | Dated decisions: direction, allowed claims, writing rules. |

Markers in the text: `[TBD: ...]` awaits data, `[pilot]` is a pilot value that
must be replaced, `[check]` is an unverified statement or citation.
