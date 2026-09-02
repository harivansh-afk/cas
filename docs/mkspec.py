"""Regenerate docs/spec.md from the playbook pages.

    uv run python docs/mkspec.py

The pages under playbook/src/routes/NN/+page.svelte are the source of truth.
This script strips them to markdown so the spec can be reviewed and hand-edited
as one file; hand edits are then ported back into the pages.
"""

from __future__ import annotations

import html
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ROUTES = ROOT / "playbook" / "src" / "routes"
OUT = ROOT / "docs" / "spec.md"

TITLES = {
    "00": "Thesis",
    "01": "Architecture",
    "02": "One host",
    "03": "Multiple hosts",
    "04": "Remote read",
    "05": "Plan",
    "06": "Prior art",
}

HEADER = """# Content Addressed Deduplication: A distributed storage system study

CS 4993, fall 2026. Research spec, v8.

This file is generated from `playbook/src/routes/00–06` by `docs/mkspec.py` for review and hand edits.
Edits here are ported back into the pages, then the file is regenerated.
Figures are described in brackets where the pages draw them.
"""


def inline(s: str) -> str:
    s = re.sub(r"<code>(.*?)</code>", r"`\1`", s, flags=re.S)
    s = re.sub(r"<(?:strong|mark)>(.*?)</(?:strong|mark)>", r"**\1**", s, flags=re.S)
    s = re.sub(r'<a href="([^"]+)"[^>]*>(.*?)</a>', r"[\2](\1)", s, flags=re.S)
    s = re.sub(r'<span class="tag-stretch">(.*?)</span>', r" (\1)", s)
    s = re.sub(r'<span class="rid">(.*?)</span>', r"\1. ", s)
    s = re.sub(r"<[^>]+>", "", s)
    s = html.unescape(s)
    return re.sub(r"[ \t]+", " ", s).strip()


def paragraph(body: str) -> str:
    lines = [inline(x) for x in re.split(r"<br\s*/?>", body)]
    return "\n\n".join(x for x in lines if x)


def table(body: str) -> str:
    rows = re.findall(r"<tr>(.*?)</tr>", body, flags=re.S)
    out = []
    for i, row in enumerate(rows):
        cells = re.findall(r"<t[hd][^>]*>(.*?)</t[hd]>", row, flags=re.S)
        cells = [inline(re.sub(r"<br\s*/?>", "; ", c)) for c in cells]
        out.append("| " + " | ".join(cells) + " |")
        if i == 0:
            out.append("|" + "---|" * len(cells))
    return "\n".join(out)


def convert(num: str, src: str) -> str:
    src = re.sub(r"<script.*?</script>", "", src, flags=re.S)
    src = re.sub(r"<PageNav[^>]*/>", "", src)
    parts = [f"# {num} {TITLES[num]}"]
    token = re.compile(
        r"<PageHead[^>]*/>"
        r"|<Diagram\b[^>]*?label=\"(?P<label>[^\"]*)\"[^>]*>.*?</Diagram>"
        r"|<h2>(?P<h2>.*?)</h2>"
        r"|<p[^>]*>(?P<p>.*?)</p>"
        r"|<(?P<lt>ul|ol)[^>]*>(?P<list>.*?)</(?P=lt)>"
        r"|<div class=\"table-scroll\">(?P<table>.*?)</div>",
        re.S,
    )
    for m in token.finditer(src):
        if m.group("label") is not None:
            parts.append(f"[figure: {inline(m.group('label'))}]")
        elif m.group("h2") is not None:
            parts.append(f"## {inline(m.group('h2'))}")
        elif m.group("p") is not None:
            parts.append(paragraph(m.group("p")))
        elif m.group("list") is not None:
            items = re.findall(r"<li>(.*?)</li>", m.group("list"), flags=re.S)
            ordered = m.group("lt") == "ol"
            parts.append(
                "\n".join(
                    (f"{i + 1}. " if ordered else "- ") + inline(re.sub(r"<br\s*/?>", " ", it))
                    for i, it in enumerate(items)
                )
            )
        elif m.group("table") is not None:
            parts.append(table(m.group("table")))
    return "\n\n".join(p for p in parts if p)


def main() -> None:
    pages = [convert(n, (ROUTES / n / "+page.svelte").read_text()) for n in TITLES]
    OUT.write_text(HEADER + "\n---\n\n" + "\n\n---\n\n".join(pages) + "\n")
    print(f"wrote {OUT.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
