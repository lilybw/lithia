#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 lithia Authors
# SPDX-License-Identifier: MPL-2.0

"""Cross-references between the design spec and the code, which cites sections as `spec §7.4`.

  spec_refs.py SECTION...   where the code cites each section or its subsections (7 covers 7.1-7.5)
  spec_refs.py --file PATH  which sections a file or folder cites
  spec_refs.py --coverage   every section with its count of citations; uncited ones of §3-§12 marked
  spec_refs.py --check      exits 1 if the code cites a section the spec does not have

Reads the newest docs/spec/LITHIA_design_spec_v*.md and the Rust files under crates/.
"""

from __future__ import annotations

import io
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT: Path = Path(__file__).resolve().parents[2]
SPEC: Path = ROOT / "docs/spec"
HEADING: re.Pattern[str] = re.compile(r"^(#{2,3}) (\d+(?:\.\d+)?)\.? (.+)$")
CITATION: re.Pattern[str] = re.compile(r"§ ?(\d+(?:\.\d+)*)")
IMPLEMENTED: range = range(3, 13)  # §3 (the API) to §12 (diagnostics) describe code


@dataclass(frozen=True)
class Citation:
    section: str
    path: str
    line: int
    text: str


def spec_file() -> Path:
    def version(path: Path) -> tuple[int, ...]:
        found: re.Match[str] | None = re.search(r"_v([\d.]+)\.md$", path.name)
        return tuple(int(n) for n in found.group(1).split(".")) if found else ()
    return max(SPEC.glob("LITHIA_design_spec_v*.md"), key=version)


def sections() -> dict[str, str]:
    """Every numbered section of the spec, with its title, in order."""
    found: dict[str, str] = {}
    for line in spec_file().read_text(encoding="utf-8").splitlines():
        heading: re.Match[str] | None = HEADING.match(line)
        if heading:
            found[heading.group(2)] = heading.group(3).strip()
    return found


def citations() -> list[Citation]:
    listed: str = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "--", "crates"],
        cwd=ROOT, capture_output=True, encoding="utf-8", errors="replace", check=True,
    ).stdout
    found: list[Citation] = []
    for name in sorted(p for p in listed.splitlines() if p.endswith(".rs")):
        path: Path = ROOT / name
        if not path.is_file():
            continue
        for number, text in enumerate(path.read_text(encoding="utf-8", errors="replace").splitlines(), 1):
            for cited in CITATION.findall(text):
                found.append(Citation(cited.rstrip("."), name, number, text.strip()))
    return found


def within(section: str, wanted: str) -> bool:
    return section == wanted or section.startswith(wanted + ".")


def main(argv: list[str]) -> int:
    if isinstance(sys.stdout, io.TextIOWrapper):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")  # Windows consoles default to a code page
    known: dict[str, str] = sections()
    cited: list[Citation] = citations()
    if argv == ["--check"]:
        unknown: list[Citation] = [c for c in cited if c.section not in known]
        for c in unknown:
            print(f"{c.path}:{c.line}: cites §{c.section}, which {spec_file().name} does not have")
        print(f"{len(cited)} citations of {len({c.section for c in cited})} sections; {len(unknown)} unknown")
        return 1 if unknown else 0
    if argv == ["--coverage"]:
        for section, title in known.items():
            count: int = sum(1 for c in cited if c.section == section)
            top: int = int(section.split(".")[0])
            flag: str = "  uncited" if not count and top in IMPLEMENTED and not has_children(section, known) else ""
            print(f"§{section:<6} {count:>3}  {title}{flag}")
        return 0
    if len(argv) == 2 and argv[0] == "--file":
        prefix: str = Path(argv[1]).resolve().relative_to(ROOT).as_posix() if Path(argv[1]).exists() else argv[1]
        mine: dict[str, int] = {}
        for c in cited:
            if c.path == prefix or c.path.startswith(prefix.rstrip("/") + "/"):
                mine[c.section] = mine.get(c.section, 0) + 1
        for section in sorted(mine, key=lambda s: tuple(int(n) for n in s.split("."))):
            print(f"§{section:<6} {mine[section]:>3}  {known.get(section, '(not in the spec)')}")
        return 0
    if argv and all(re.fullmatch(r"\d+(\.\d+)*", a.lstrip("§")) for a in argv):
        for wanted in (a.lstrip("§") for a in argv):
            print(f"§{wanted} {known.get(wanted, '(not in the spec)')}")
            for c in cited:
                if within(c.section, wanted):
                    print(f"  {c.path}:{c.line}: {c.text[:140]}")
        return 0
    print(__doc__, file=sys.stderr)
    return 2


def has_children(section: str, known: dict[str, str]) -> bool:
    return any(other.startswith(section + ".") for other in known)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
