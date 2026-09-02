#!/usr/bin/env python3
"""Build spec.pdf from the prerendered site.

Reads build/00.html … build/06.html, lifts the article out of each page,
turns every inline SVG figure into a PDF via rsvg-convert, and hands one
HTML document to pandoc, which writes LaTeX. latexmk then produces the PDF.

The pages are the single source: prose, tables, and figures all come from
the same build that serves the site.

Usage: python3 scripts/pdf/build.py [--out build/spec.pdf]
Run after `pnpm build`.
"""

from __future__ import annotations

import argparse
import html
import os
from datetime import date
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BUILD = ROOT / "build"
HERE = Path(__file__).resolve().parent
WORK = ROOT / ".svelte-kit" / "pdf"
WOFF2 = ROOT / "src" / "lib" / "assets" / "fonts" / "BerkeleyMono-Variable.woff2"

PAGES = ["00", "01", "02", "03", "04", "05", "06"]
TEXT = "#1c1917"  # site --text-primary, light scheme
MONO = "Berkeley Mono"


def run(cmd: list[str], **kw) -> None:
    print("+", " ".join(str(c) for c in cmd), file=sys.stderr)
    subprocess.run(cmd, check=True, **kw)


def article(page: str) -> str:
    src = (BUILD / f"{page}.html").read_text()
    m = re.search(r"<main>(.*)</main>", src, re.S)
    if not m:
        sys.exit(f"{page}: no <main>")
    body = m.group(1)
    body = re.sub(r"<!--.*?-->", "", body, flags=re.S)
    body = re.sub(r"<script.*?</script>", "", body, flags=re.S)
    body = re.sub(r'<header class="site.*?</header>', "", body, flags=re.S)
    body = re.sub(r'<span class="eyebrow">.*?</span>', "", body, flags=re.S)
    body = re.sub(r'<nav class="pagenav.*?</nav>', "", body, flags=re.S)
    return body


def font_file() -> Path:
    """Decompress the site's woff2 to a TTF that fontspec and rsvg can load."""
    out = WORK / "fonts" / "BerkeleyMono-Variable.ttf"
    if out.exists():
        return out
    out.parent.mkdir(parents=True, exist_ok=True)
    run(
        [
            "uv", "run", "--quiet", "--with", "fonttools", "--with", "brotli",
            "python", "-m", "fontTools.ttLib.woff2", "decompress", "-o", str(out), str(WOFF2),
        ]
    )
    return out


def fontconfig(ttf: Path) -> dict[str, str]:
    """An environment in which fontconfig, and so rsvg-convert, can find the TTF."""
    conf = WORK / "fonts.conf"
    conf.write_text(
        "<?xml version='1.0'?><!DOCTYPE fontconfig SYSTEM 'fonts.dtd'>"
        f"<fontconfig><dir>{ttf.parent}</dir><include ignore_missing='yes'>/etc/fonts/fonts.conf</include>"
        f"<cachedir>{WORK / 'fc-cache'}</cachedir></fontconfig>"
    )
    return {**os.environ, "FONTCONFIG_FILE": str(conf)}


def figures(page: str, body: str, figdir: Path, fontenv: dict[str, str]) -> str:
    """Replace each <figure><svg/><figcaption/></figure> with an <img> to a PDF."""
    n = 0

    def repl(m: re.Match) -> str:
        nonlocal n
        n += 1
        svg, cap = m.group(1), m.group(2) or ""
        name = f"fig-{page}-{n}"
        vb = re.search(r'viewBox="0 0 (\d+) (\d+)"', svg)
        w, h = (vb.group(1), vb.group(2)) if vb else ("1000", "400")
        svg = re.sub(r"<svg ", f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" ', svg, count=1)
        svg = re.sub(r' style="[^"]*"', "", svg, count=1)
        svg = svg.replace("<defs>", f"<style>text{{font-family:'{MONO}',monospace}}</style><defs>", 1)
        # currentColor has no value outside a page; pin it to the site's text colour.
        svg = svg.replace("currentColor", TEXT)
        (figdir / f"{name}.svg").write_text(svg)
        run(["rsvg-convert", "-f", "pdf", "-o", str(figdir / f"{name}.pdf"), str(figdir / f"{name}.svg")], env=fontenv)
        cap = re.sub(r"<[^>]+>", "", cap)
        return f'<figure><img src="{figdir / name}.pdf" alt="{html.escape(cap, quote=True)}" /><figcaption>{cap}</figcaption></figure>'

    return re.sub(r"<figure>\s*(<svg.*?</svg>)\s*(?:<figcaption>(.*?)</figcaption>)?\s*</figure>", repl, body, flags=re.S)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=str(BUILD / "spec.pdf"))
    args = ap.parse_args()

    if not (BUILD / "00.html").exists():
        sys.exit("run `pnpm build` first")
    for tool in ("pandoc", "rsvg-convert", "latexmk", "xelatex", "uv"):
        if not shutil.which(tool):
            sys.exit(f"missing {tool}")

    figdir = WORK / "fig"
    figdir.mkdir(parents=True, exist_ok=True)
    ttf = font_file()
    fontenv = fontconfig(ttf)

    parts = [figures(p, article(p), figdir, fontenv) for p in PAGES]
    doc = WORK / "spec.html"
    doc.write_text("\n".join(parts))

    idx = (BUILD / "index.html").read_text()
    title = html.unescape(re.search(r"<title>(.*?)</title>", idx).group(1))
    desc = html.unescape(re.search(r'name="description" content="(.*?)"', idx).group(1))

    tex = WORK / "spec.tex"
    run(
        [
            "pandoc", str(doc), "-f", "html", "-t", "latex", "-s", "-o", str(tex),
            "--lua-filter", str(HERE / "filter.lua"),
            "-H", str(HERE / "header.tex"),
            "--number-sections", "--toc", "--toc-depth=2",
            "-M", f"title={title}",
            "-M", "author=Harivansh Rathi",
            "-M", "subtitle=CS 4993 · Research specification",
            "-M", f"abstract={desc}",
            "-M", f"date={date.today():%B %Y}",
            "-V", "mainfont=TeX Gyre Pagella",
            "-V", f"monofont={ttf.name}",
            "-V", f"monofontoptions=Path={ttf.parent}/",
            "-V", "fontsize=10pt",
            "-V", "geometry:margin=1in",
            "-V", "colorlinks=true", "-V", "linkcolor=black", "-V", "urlcolor=black", "-V", "toccolor=black",
            "-V", "hyperrefoptions=hidelinks",
        ]
    )
    run(["latexmk", "-xelatex", "-interaction=nonstopmode", "-halt-on-error", f"-output-directory={WORK}", str(tex)],
        stdout=subprocess.DEVNULL)
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy(WORK / "spec.pdf", out)
    print(out)


if __name__ == "__main__":
    main()
