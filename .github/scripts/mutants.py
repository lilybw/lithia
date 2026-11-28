#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 lithia Authors
# SPDX-License-Identifier: MPL-2.0

"""Runs cargo-mutants in place on a few files, as the weekly job does, and summarises what it found.

  mutants.py FILE... [--re REGEX] [--list] [-- CARGO_TEST_ARGS...]

In place means the warm `target/` is reused, so a handful of mutants takes minutes, not hours. The
files are saved first and put back afterwards even if cargo-mutants is interrupted. Without test
arguments it runs the tests the weekly job runs: `--skip compile_errors` over lithia,
lithia-validation and lithia-test. Results stay in target/mutants.out.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
from pathlib import Path

ROOT: Path = Path(__file__).resolve().parents[2]
OUT: Path = ROOT / "target/mutants.out"
VERSION: str = "27.1.0"
PACKAGES: str = "lithia,lithia-validation,lithia-test"


def main(argv: list[str]) -> int:
    split: int = argv.index("--") if "--" in argv else len(argv)
    own: list[str] = argv[:split]
    test_args: list[str] = argv[split + 1 :] or ["--", "--skip", "compile_errors"]
    files: list[str] = []
    selection: list[str] = []
    listing: bool = False
    while own:
        arg: str = own.pop(0)
        if arg == "--re" and own:
            selection += ["--re", own.pop(0)]
        elif arg == "--list":
            listing = True
        elif (ROOT / arg).is_file():
            files.append(arg)
        else:
            print(f"not a file: {arg}\n{__doc__}", file=sys.stderr)
            return 2
    if not files:
        print(__doc__, file=sys.stderr)
        return 2
    if subprocess.run(["cargo", "mutants", "--version"], capture_output=True).returncode != 0:
        print(f"cargo-mutants is missing: cargo install cargo-mutants --locked --version {VERSION}", file=sys.stderr)
        return 2
    command: list[str] = ["cargo", "mutants", "--no-shuffle"]
    for f in files:
        command += ["--file", f]
    command += selection
    if listing:
        return subprocess.run([*command, "--list"], cwd=ROOT).returncode
    saved: dict[str, bytes] = {f: (ROOT / f).read_bytes() for f in files}
    command += ["--in-place", "--timeout", "600", "--test-package", PACKAGES, "--output", str(ROOT / "target"),
                "--", *test_args]
    try:
        subprocess.run(command, cwd=ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    finally:
        restored: list[str] = [f for f, text in saved.items() if (ROOT / f).read_bytes() != text]
        for f in restored:
            (ROOT / f).write_bytes(saved[f])
        if restored:
            print(f"put back mutated files: {', '.join(restored)}", file=sys.stderr)
    return report()


def report() -> int:
    try:
        outcomes: dict[str, object] = json.loads((OUT / "outcomes.json").read_text())
    except (OSError, ValueError):
        print("cargo-mutants wrote no outcomes; see target/mutants.out/debug.log", file=sys.stderr)
        return 1
    counts: list[str] = [f"{key} {outcomes[key]}" for key in ("caught", "missed", "timeout", "unviable") if key in outcomes]
    print(f"{outcomes.get('total_mutants', '?')} mutants: {', '.join(counts)}")
    missed: Path = OUT / "missed.txt"
    for line in (missed.read_text().splitlines() if missed.is_file() else []):
        print(f"  missed: {line}")
    baseline: object = outcomes.get("outcomes")
    if isinstance(baseline, list) and baseline and isinstance(baseline[0], dict):
        summary: object = baseline[0].get("summary")
        if summary not in (None, "Success"):
            print(f"  the unmutated tests did not pass ({summary}): fix them first", file=sys.stderr)
            return 1
    return 0 if not missed.is_file() or not missed.read_text().strip() else 1


if __name__ == "__main__":
    if "--report" in sys.argv[1:]:
        sys.exit(report())
    if shutil.which("cargo") is None:
        sys.exit("cargo is missing")
    sys.exit(main(sys.argv[1:]))
