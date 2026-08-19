#!/usr/bin/env python3
"""Render a Markdown file to a standalone, lightly-styled HTML page.

Used by the docs workflow to turn the generated CLI reference (docs/cli.md) into
an HTML page for the published site. Usage: md_to_html.py IN.md OUT.html [TITLE]
"""

from __future__ import annotations

import sys
from pathlib import Path

import markdown  # pip install markdown

STYLE = """
:root { color-scheme: light dark; }
body { font: 16px/1.6 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
       max-width: 60rem; margin: 0 auto; padding: 2rem 1.25rem; }
h1, h2, h3 { line-height: 1.25; }
h3 { margin-top: 2rem; border-bottom: 1px solid #8883; padding-bottom: .2rem; }
code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: .9em;
       background: #8881; padding: .1em .35em; border-radius: .3em; }
table { border-collapse: collapse; width: 100%; margin: .75rem 0; font-size: .92rem; }
th, td { border: 1px solid #8883; padding: .35rem .55rem; text-align: left; vertical-align: top; }
th { background: #8881; }
a { color: #4a90d9; }
p > a[href^="#"] { text-decoration: none; }
"""

TEMPLATE = """<!doctype html>
<html lang="en"><head><meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>{title}</title><style>{style}</style></head>
<body><p><a href="../">&larr; owlmake docs</a></p>{body}</body></html>
"""


def main() -> int:
    if len(sys.argv) < 3:
        raise SystemExit("usage: md_to_html.py IN.md OUT.html [TITLE]")
    src, dst = Path(sys.argv[1]), Path(sys.argv[2])
    title = sys.argv[3] if len(sys.argv) > 3 else "owlmake"
    body = markdown.markdown(
        src.read_text(),
        extensions=["tables", "fenced_code", "toc"],
    )
    dst.parent.mkdir(parents=True, exist_ok=True)
    dst.write_text(TEMPLATE.format(title=title, style=STYLE, body=body))
    print(f"wrote {dst}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
