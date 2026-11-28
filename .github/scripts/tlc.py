#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 lithia Authors
# SPDX-License-Identifier: MPL-2.0

"""Runs TLC on models and checks each against its recorded state count, one summary line per model.

  tlc.py MODEL... [--record] [--workers N]

A model whose distinct-state count or depth differs from `docs/tla/models/counts.tsv` fails, so a
change to a model or the spec shows up; `--record` writes the new figures instead. A model recorded
as `bounded MINUTES` runs that long and passes if TLC finds no error within it. The full output of
each run goes to `target/tlc/MODEL.log`; a failure also prints TLC's error and the start of its trace.
"""

from __future__ import annotations

import io
import os
import re
import signal
import subprocess
import sys
import time
from pathlib import Path

ROOT: Path = Path(__file__).resolve().parents[2]
MODELS: Path = ROOT / "docs/tla/models"
COUNTS: Path = MODELS / "counts.tsv"
LOGS: Path = ROOT / "target/tlc"
ERRORS: re.Pattern[str] = re.compile(r"^Error:|is violated|Deadlock reached|Temporal properties were violated|^TLC threw")


def read_counts() -> dict[str, list[str]]:
    rows: dict[str, list[str]] = {}
    for line in COUNTS.read_text(encoding="utf-8").splitlines():
        if line.strip() and not line.startswith("#"):
            fields: list[str] = line.split("\t")
            rows[fields[0]] = fields[1:]
    return rows


def write_counts(rows: dict[str, list[str]]) -> None:
    header: list[str] = [line for line in COUNTS.read_text(encoding="utf-8").splitlines() if line.startswith("#")]
    body: list[str] = ["\t".join([model, *rows[model]]) for model in sorted(rows)]
    COUNTS.write_text("\n".join(header + body) + "\n", encoding="utf-8", newline="\n")


def run(model: str, workers: str, minutes: int | None) -> tuple[str, float]:
    """TLC's whole output for `model`, and the seconds it took; stops it after `minutes` if given."""
    LOGS.mkdir(parents=True, exist_ok=True)
    started: float = time.monotonic()
    # Through bash, which Windows has with Git for Windows; tlc.sh fetches the jar and starts Java.
    command: list[str] = ["bash", (ROOT / "docs/tla/tlc.sh").as_posix(), model, "-workers", workers]
    tlc: subprocess.Popen[str]
    # A process group of its own, so a timeout stops Java with the script that started it.
    if sys.platform == "win32":
        tlc = subprocess.Popen(
            command, cwd=MODELS, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, encoding="utf-8",
            errors="replace", creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
        )
    else:
        tlc = subprocess.Popen(
            command, cwd=MODELS, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, encoding="utf-8",
            errors="replace", start_new_session=True,
        )
    output: str
    try:
        output = tlc.communicate(timeout=minutes * 60 if minutes else None)[0]
    except subprocess.TimeoutExpired:
        if sys.platform == "win32":
            subprocess.run(["taskkill", "/F", "/T", "/PID", str(tlc.pid)], capture_output=True)
        else:
            os.killpg(tlc.pid, signal.SIGTERM)
        output = tlc.communicate()[0] + f"\n(stopped after {minutes} minutes)\n"
    output = "\n".join(line for line in output.splitlines() if not line.startswith("Picked up JAVA_TOOL_OPTIONS"))
    (LOGS / f"{model}.log").write_text(output, encoding="utf-8", newline="\n")
    return output, time.monotonic() - started


def check(model: str, recorded: list[str] | None, record: bool, workers: str) -> tuple[bool, list[str] | None]:
    """Runs `model`, prints its summary line; returns whether it passed and the figures to record."""
    bounded: int | None = int(recorded[1]) if recorded and recorded[0] == "bounded" else None
    output, seconds = run(model, workers, bounded)
    errors: list[str] = [line for line in output.splitlines() if ERRORS.search(line)]
    states: list[str] = re.findall(r"([\d,]+) distinct states found", output)
    depth: list[str] = re.findall(r"depth of the complete state graph search is (\d+)", output)
    took: str = f"{seconds / 60:.0f} min" if seconds >= 120 else f"{seconds:.0f} s"
    if errors:
        print(f"FAIL     {model}: {errors[0]} ({took}; log target/tlc/{model}.log)")
        lines: list[str] = output.splitlines()
        first: int = next(i for i, line in enumerate(lines) if ERRORS.search(line))
        print("\n".join(lines[first : first + 150]))
        return False, None
    if bounded is not None:
        reached: str = states[-1] if states else "?"
        print(f"ok       {model}: no error within {bounded} min ({reached} distinct states)")
        return True, recorded
    if not states or not depth:
        print(f"FAIL     {model}: TLC did not finish ({took}; log target/tlc/{model}.log)")
        print("\n".join(output.splitlines()[-30:]))
        return False, None
    found: list[str] = [states[-1].replace(",", ""), depth[-1]]
    figures: str = f"{int(found[0]):,}".replace(",", " ") + f" states, depth {found[1]}"
    if recorded == found:
        print(f"ok       {model}: {figures} ({took})")
        return True, found
    if record:
        print(f"recorded {model}: {figures} ({took})" + (f", was {' / '.join(recorded)}" if recorded else ""))
        return True, found
    if recorded is None:
        print(f"new      {model}: {figures} ({took}); not in counts.tsv, add it with --record")
        return True, None
    print(f"CHANGED  {model}: {figures}, recorded {recorded[0]} states, depth {recorded[1]} ({took});"
          " if the model changed on purpose, rerun with --record")
    return False, None


def main(argv: list[str]) -> int:
    if isinstance(sys.stdout, io.TextIOWrapper):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")  # Windows consoles default to a code page
    record: bool = "--record" in argv
    workers: str = "auto"
    models: list[str] = []
    args: list[str] = [a for a in argv if a != "--record"]
    while args:
        arg: str = args.pop(0)
        if arg == "--workers" and args:
            workers = args.pop(0)
        elif re.fullmatch(r"MC\w+", arg) and (MODELS / f"{arg}.tla").is_file():
            models.append(arg)
        else:
            print(f"not a model: {arg}\n{__doc__}", file=sys.stderr)
            return 2
    if not models:
        print(__doc__, file=sys.stderr)
        return 2
    rows: dict[str, list[str]] = read_counts()
    passed: bool = True
    for model in models:
        ok, found = check(model, rows.get(model), record, workers)
        passed = passed and ok
        if record and found is not None:
            rows[model] = found
    if record:
        write_counts(rows)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
