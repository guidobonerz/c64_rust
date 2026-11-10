"""Builds docs/rust64-guide.pdf from docs/rust64-guide.md.

Needs: pip install markdown pygments, and Chrome or Edge (headless printing).

    python docs/make_pdf.py [--chrome PATH]
"""

import argparse
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile

import markdown
from pygments.formatters import HtmlFormatter

HERE = pathlib.Path(__file__).resolve().parent
SRC = HERE / "rust64-guide.md"
OUT = HERE / "rust64-guide.pdf"

CSS = """
@page { size: A4; margin: 18mm 16mm 18mm 16mm; }
body { font-family: "Segoe UI", "Helvetica Neue", Arial, sans-serif; font-size: 10pt;
       line-height: 1.45; color: #1d1d1f; }
h1 { font-size: 22pt; margin: 0 0 4pt 0; color: #1a3a6b; }
h2 { font-size: 15pt; color: #1a3a6b; border-bottom: 1.5pt solid #1a3a6b; padding-bottom: 2pt;
     margin-top: 18pt; break-before: page; }
h3 { font-size: 12pt; color: #1a3a6b; margin-top: 14pt; break-after: avoid; }
p, li { orphans: 3; widows: 3; }
a { color: #1a5fb4; text-decoration: none; }
code { font-family: "Cascadia Mono", Consolas, "Courier New", monospace; font-size: 8.6pt;
       background: #f0f2f5; padding: 0 2pt; border-radius: 2pt; }
pre { background: #f6f8fa; border: 0.5pt solid #d0d7de; border-left: 3pt solid #1a3a6b;
      padding: 6pt 8pt; border-radius: 3pt; overflow: hidden; white-space: pre-wrap;
      break-inside: avoid; font-size: 8.4pt; line-height: 1.35; }
pre code { background: none; padding: 0; font-size: 8.4pt; }
table { border-collapse: collapse; margin: 6pt 0; width: 100%; break-inside: auto; font-size: 9pt; }
th, td { border: 0.5pt solid #c8ccd2; padding: 3pt 5pt; vertical-align: top; text-align: left; }
th { background: #e8edf5; }
tr { break-inside: avoid; }
hr { display: none; }
.title { height: 250mm; display: flex; flex-direction: column; justify-content: center; }
.title h1 { font-size: 34pt; border: none; }
.title .sub { font-size: 15pt; color: #555; margin-bottom: 18pt; }
.title .meta { font-size: 10pt; color: #777; }
"""


def find_browser(explicit):
    if explicit:
        return explicit
    candidates = [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    ]
    for name in ("google-chrome", "chromium", "chromium-browser", "chrome", "msedge"):
        p = shutil.which(name)
        if p:
            candidates.insert(0, p)
    for c in candidates:
        if os.path.exists(c):
            return c
    sys.exit("Chrome or Edge not found; pass --chrome PATH")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--chrome")
    ap.add_argument("--html", help="also keep the HTML here")
    args = ap.parse_args()

    text = SRC.read_text(encoding="utf-8")
    body = markdown.markdown(
        text,
        extensions=["tables", "fenced_code", "codehilite", "toc"],
        extension_configs={"codehilite": {"guess_lang": False, "css_class": "hl"}},
    )
    # the first heading becomes a title page
    first, rest = body.split("</h1>", 1)
    title = first.split(">", 1)[1] if ">" in first else "rust64"
    title_page = (
        '<div class="title"><h1>%s</h1><div class="sub">Rust for the Commodore 64</div>'
        '<div class="meta">Codegen backend, the c64 library and a cookbook</div></div>' % title
    )
    css = CSS + HtmlFormatter(style="friendly").get_style_defs(".hl")
    html = (
        '<!doctype html><html><head><meta charset="utf-8"><title>rust64 User Guide</title>'
        "<style>%s</style></head><body>%s%s</body></html>" % (css, title_page, rest)
    )
    with tempfile.TemporaryDirectory() as tmp:
        page = pathlib.Path(args.html) if args.html else pathlib.Path(tmp) / "guide.html"
        page.write_text(html, encoding="utf-8")
        browser = find_browser(args.chrome)
        cmd = [
            browser,
            "--headless=new",
            "--disable-gpu",
            "--no-pdf-header-footer",
            "--user-data-dir=" + str(pathlib.Path(tmp) / "profile"),
            "--print-to-pdf=" + str(OUT),
            page.resolve().as_uri(),
        ]
        subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=120)
    print("wrote", OUT)


if __name__ == "__main__":
    main()
