#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 lithia Authors
# SPDX-License-Identifier: MPL-2.0

"""What a session needs first: the branch, which tools are present, and a map of the code."""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from outline import ROOT, Index, update  # noqa: E402

TOOLS: dict[str, list[str]] = {
    "cargo": ["cargo", "--version"],
    "java (TLC)": ["java", "-version"],
    "cargo-mutants": ["cargo", "mutants", "--version"],
    "pandoc": ["pandoc", "--version"],
    "mypy": ["mypy", "--version"],
    "gh": ["gh", "--version"],
}


def git(*args: str) -> str:
    done: subprocess.CompletedProcess[str] = subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True)
    return done.stdout.strip()


def present(command: list[str]) -> bool:
    if shutil.which(command[0]) is None:
        return False
    return subprocess.run(command, capture_output=True).returncode == 0


def main() -> int:
    branch: str = git("branch", "--show-current") or "(detached)"
    head: str = git("log", "-1", "--format=%h %s")
    dirty: int = len([line for line in git("status", "--porcelain").splitlines() if line])
    print(f"lithia: branch {branch} at {head}" + (f"; {dirty} uncommitted path(s)" if dirty else ""))
    missing: list[str] = [name for name, command in TOOLS.items() if not present(command)]
    if missing:
        print(f"Missing tools: {', '.join(missing)} (cargo-mutants: cargo install cargo-mutants --locked --version 27.1.0)")
    index: Index
    index, _ = update()
    print("Code map; `python3 .claude/tools/outline.py show FOLDER` outlines one:")
    for folder in sorted(index):
        entry: dict[str, str | int] = index[folder]
        about: str = str(entry["summary"])
        about = about if len(about) <= 90 else about[:87] + "..."
        print(f"  {folder:<30} {entry['lines']:>5} lines  {about}")
    print("Checks: python3 .github/scripts/check.py [full]; TLC: .github/scripts/tlc.py MODEL. See CLAUDE.md.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
