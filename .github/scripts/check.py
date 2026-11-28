#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 lithia Authors
# SPDX-License-Identifier: MPL-2.0

"""Runs the checks a change must pass, one summary line per step; full output goes to target/check/.

  check.py [quick|full] [--only STEP,...]

`quick` (the default): fmt, clippy, rustdoc, the workspace tests and the harness tools.
`full`: also clippy under `--cfg lithia_loom` and the loom models with and without the fast path.
Steps: fmt, clippy, clippy-loom, doc, test, loom, loom-slow, tools. Exits 1 if any step fails.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path

ROOT: Path = Path(__file__).resolve().parents[2]
LOGS: Path = ROOT / "target/check"
LOOM: dict[str, str] = {"RUSTFLAGS": "--cfg lithia_loom", "CARGO_TARGET_DIR": "target/loom"}
PYTHON: list[str] = [".github/scripts", ".claude/tools"]


@dataclass(frozen=True)
class Step:
    name: str
    command: list[str]
    env: dict[str, str] = field(default_factory=dict)


STEPS: list[Step] = [
    Step("fmt", ["cargo", "fmt", "--all", "--check"]),
    Step("clippy", ["cargo", "clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"]),
    Step("clippy-loom", ["cargo", "clippy", "-p", "lithia", "-p", "lithia-test", "--all-targets", "--locked", "--",
                         "-D", "warnings"], LOOM),
    Step("doc", ["cargo", "doc", "--no-deps", "-p", "lithia", "-p", "lithia-macro", "--locked"],
         {"RUSTDOCFLAGS": "-D warnings"}),
    Step("test", ["cargo", "test", "--workspace", "--locked"]),
    Step("loom", ["cargo", "test", "-p", "lithia-test", "--test", "loom", "--release", "--locked"], LOOM),
    Step("loom-slow", ["cargo", "test", "-p", "lithia-test", "--test", "loom", "--release", "--locked"],
         {**LOOM, "LITHIA_LOOM_FAST_PATH": "off"}),
    Step("tools", ["python3", "-m", "unittest", "discover", "-s", ".claude/tools", "-q"]),
]
QUICK: list[str] = ["fmt", "clippy", "doc", "test", "tools"]


def summary(step: Step, output: str) -> str:
    """What a step's output says, in a few words."""
    results: list[tuple[int, int, int]] = [
        (int(m[0]), int(m[1]), int(m[2]))
        for m in re.findall(r"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored", output)
    ]
    if results:
        passed: int = sum(r[0] for r in results)
        failed: int = sum(r[1] for r in results)
        ignored: int = sum(r[2] for r in results)
        return f"{passed} passed, {failed} failed" + (f", {ignored} ignored" if ignored else "")
    ran: re.Match[str] | None = re.search(r"Ran (\d+) tests?", output)
    return f"{ran.group(1)} tests" if ran else ""


def failures(output: str) -> list[str]:
    """The lines worth reading first when a step fails."""
    named: list[str] = re.findall(r"^test (\S+) \.\.\. FAILED", output, re.M)
    if named:
        return [f"failed: {name}" for name in named[:20]]
    errors: list[str] = [line for line in output.splitlines() if re.match(r"^(error|warning)(\[\w+\])?:|^Diff in|^FAIL", line)]
    return errors[:20] or output.splitlines()[-15:]


def run(step: Step) -> bool:
    LOGS.mkdir(parents=True, exist_ok=True)
    started: float = time.monotonic()
    done: subprocess.CompletedProcess[str] = subprocess.run(
        step.command, cwd=ROOT, env={**os.environ, **step.env}, capture_output=True, text=True,
    )
    output: str = done.stdout + done.stderr
    (LOGS / f"{step.name}.log").write_text(output)
    seconds: float = time.monotonic() - started
    took: str = f"{seconds / 60:.1f} min" if seconds >= 120 else f"{seconds:.0f} s"
    said: str = summary(step, output)
    if done.returncode == 0:
        print(f"ok    {step.name:<12} {took:>8}  {said}")
        return True
    print(f"FAIL  {step.name:<12} {took:>8}  {said}  (log target/check/{step.name}.log)")
    for line in failures(output):
        print(f"      {line}")
    return False


def main(argv: list[str]) -> int:
    names: list[str] = QUICK
    args: list[str] = list(argv)
    if args and args[0] in ("quick", "full"):
        names = QUICK if args.pop(0) == "quick" else [s.name for s in STEPS]
    if len(args) == 2 and args[0] == "--only":
        names = args[1].split(",")
    elif args:
        print(__doc__, file=sys.stderr)
        return 2
    unknown: list[str] = [n for n in names if n not in {s.name for s in STEPS}]
    if unknown:
        print(f"unknown steps: {', '.join(unknown)}\n{__doc__}", file=sys.stderr)
        return 2
    if "tools" in names and shutil.which("mypy"):
        STEPS.append(Step("mypy", ["mypy", "--strict", *PYTHON]))
        names = [*names, "mypy"]
    ok: bool = True
    for step in STEPS:
        if step.name in names:
            ok = run(step) and ok
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
