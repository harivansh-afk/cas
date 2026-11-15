#!/usr/bin/env bash
# Build paper.pdf from paper.tex and refs.bib with TeX Live from the
# repository's pinned nixpkgs. Usage: docs/paper/build.sh
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cp "$here/paper.tex" "$here/refs.bib" "$tmp/"
nix shell --inputs-from "$repo" nixpkgs#texliveSmall -c sh -c "
  cd '$tmp' &&
  pdflatex -interaction=nonstopmode -halt-on-error paper.tex >/dev/null &&
  bibtex paper >/dev/null &&
  pdflatex -interaction=nonstopmode -halt-on-error paper.tex >/dev/null &&
  pdflatex -interaction=nonstopmode -halt-on-error paper.tex > build.log ||
  { tail -40 paper.log; exit 1; }
  grep -E 'Warning.*(undefined|Citation)' paper.log || true
  grep -E '^Output written' paper.log
"
cp "$tmp/paper.pdf" "$here/paper.pdf"
