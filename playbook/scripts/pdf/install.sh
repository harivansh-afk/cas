#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tools="$root/.svelte-kit/pdf-tools"

if command -v dnf >/dev/null; then
  dnf install -y --setopt=install_weak_deps=False \
    tar gzip python3 latexmk librsvg2-tools poppler-utils \
    texlive-xetex texlive-latex texlive-amsmath texlive-amsfonts \
    texlive-booktabs texlive-caption texlive-float texlive-microtype \
    texlive-titlesec texlive-geometry texlive-fontspec texlive-unicode-math \
    texlive-tex-gyre texlive-tex-gyre-math texlive-lm texlive-hyperref \
    texlive-bookmark texlive-xurl texlive-footnotehyper texlive-etoolbox \
    texlive-tools texlive-upquote texlive-fancyvrb texlive-xcolor
elif command -v apt-get >/dev/null; then
  elevate=()
  if [ "$(id -u)" -ne 0 ]; then elevate=(sudo); fi
  "${elevate[@]}" apt-get update -q
  "${elevate[@]}" apt-get install -y --no-install-recommends curl ca-certificates \
    python3 librsvg2-bin poppler-utils latexmk texlive-xetex \
    texlive-latex-recommended texlive-latex-extra texlive-fonts-recommended \
    lmodern fonts-texgyre
else
  printf '%s\n' 'This CI bootstrap supports Amazon Linux and Ubuntu.' >&2
  exit 1
fi

case "$(uname -m)" in
  x86_64)
    pandoc_arch=amd64
    uv_arch=x86_64
    pandoc_sha=8f8f67fdd540b6519326b0ac49d5c55c5d5d15e43920e80a086e02c8aff83268
    uv_sha=741ff1f5742c5a4a25d2f829e8395355e43f7a5ae2ebc6368e9ae2df0efb69cf
    ;;
  aarch64)
    pandoc_arch=arm64
    uv_arch=aarch64
    pandoc_sha=4ef2997ff0fa7f86ada5a217722f4f732293e38518b4442ececce16628bd0e44
    uv_sha=726b72a137fda33565143325f7d31c42cd30ff9ccdf067e00d124d37b4081cb2
    ;;
  *) printf '%s\n' 'Unsupported PDF build architecture.' >&2; exit 1 ;;
esac

mkdir -p "$tools/bin"
curl --fail --location --retry 3 --output "$tools/pandoc.tar.gz" \
  "https://github.com/jgm/pandoc/releases/download/3.7.0.2/pandoc-3.7.0.2-linux-$pandoc_arch.tar.gz"
printf '%s  %s\n' "$pandoc_sha" "$tools/pandoc.tar.gz" | sha256sum --check -
tar -xzf "$tools/pandoc.tar.gz" --strip-components=1 -C "$tools"
curl --fail --location --retry 3 --output "$tools/uv.tar.gz" \
  "https://github.com/astral-sh/uv/releases/download/0.8.22/uv-$uv_arch-unknown-linux-gnu.tar.gz"
printf '%s  %s\n' "$uv_sha" "$tools/uv.tar.gz" | sha256sum --check -
tar -xzf "$tools/uv.tar.gz" --strip-components=1 -C "$tools/bin"
"$tools/bin/pandoc" --version
"$tools/bin/uv" --version
