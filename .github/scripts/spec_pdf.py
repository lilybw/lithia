#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 lithia Authors
# SPDX-License-Identifier: MPL-2.0

"""Prints the newest design spec in docs/spec to the PDF beside it: pandoc's HTML with
docs/spec/print.css, printed by Chromium through Playwright. Needs pandoc and `pip install playwright`.

  spec_pdf.py [SPEC.md]
"""

from __future__ import annotations

import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from playwright.sync_api import Browser, Page, sync_playwright

ROOT: Path = Path(__file__).resolve().parents[2]
SPEC: Path = ROOT / "docs/spec"


def newest() -> Path:
    """The spec with the highest version in its name."""
    def version(path: Path) -> tuple[int, ...]:
        found: re.Match[str] | None = re.search(r"_v([\d.]+)\.md$", path.name)
        return tuple(int(n) for n in found.group(1).split(".")) if found else ()
    return max(SPEC.glob("LITHIA_design_spec_v*.md"), key=version)


def main(argv: list[str]) -> int:
    source: Path = Path(argv[0]).resolve() if argv else newest()
    title: str = source.read_text(encoding="utf-8").splitlines()[0].lstrip("# ").strip()
    pdf: Path = source.with_suffix(".pdf")
    with tempfile.TemporaryDirectory() as scratch:
        page_file: Path = Path(scratch) / "spec.html"
        subprocess.run(
            ["pandoc", source.name, "--from", "gfm", "--to", "html5", "--standalone", "--embed-resources",
             "--css", str(SPEC / "print.css"), "--metadata", f"pagetitle={title}", "--resource-path=.",
             "-o", str(page_file)],
            cwd=source.parent, check=True,
        )
        with sync_playwright() as playwright:
            browser: Browser = playwright.chromium.launch()
            page: Page = browser.new_page()
            page.goto(page_file.as_uri())
            page.wait_for_load_state("networkidle")
            page.emulate_media(media="print", color_scheme="light")
            page.pdf(path=str(pdf), format="A4", print_background=False,
                     margin={"top": "18mm", "bottom": "18mm", "left": "16mm", "right": "16mm"})
            browser.close()
    pages: str = ""
    if shutil.which("pdfinfo"):
        pages = subprocess.run(["pdfinfo", str(pdf)], capture_output=True, encoding="utf-8", errors="replace").stdout
    count: re.Match[str] | None = re.search(r"^Pages:\s+(\d+)", pages, re.M)
    print(f"{pdf.relative_to(ROOT)}" + (f": {count.group(1)} pages" if count else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
