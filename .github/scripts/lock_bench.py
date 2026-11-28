#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 lithia Authors
# SPDX-License-Identifier: MPL-2.0

"""Runs the scaling benchmark once per sweep lock and prints one table per row, a lock per line.

  lock_bench.py [--locks parking_lot,fair,std,spin,ticket] [--rows last-handle,sweeps,rebuild]
                [--threads 1,2,4,...]
  lock_bench.py --check    lints lithia built with each lock, as `check.py full` does

Each lock is a build with `--cfg lithia_sweep_lock="NAME"` in its own target directory under
target/locks, so switching between them rebuilds nothing twice. The tables also go to
target/locks/results.md. The locks (crates/lithia/src/lifetime/sweep_lock.rs):

  parking_lot  parking_lot's mutex, unfair unlocks (the default build)
  fair         parking_lot's mutex, every unlock handed to the next waiter
  std          the standard library's mutex (a futex on Linux, an SRW lock on Windows)
  spin         parking_lot's mutex, tried for about ten microseconds before parking
  ticket       a ticket lock: first come, first served, spinning then yielding, never parking
"""

from __future__ import annotations

import io
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT: Path = Path(__file__).resolve().parents[2]
OUT: Path = ROOT / "target/locks"
LOCKS: list[str] = ["parking_lot", "fair", "std", "spin", "ticket"]
ROWS: list[str] = ["last-handle", "sweeps", "rebuild"]
Table = dict[str, list[str]]


def option(argv: list[str], name: str, default: list[str]) -> list[str]:
    """The comma-separated values after `name`, or `default`."""
    if name in argv:
        index: int = argv.index(name)
        if index + 1 >= len(argv):
            raise SystemExit(f"{name} needs a value\n{__doc__}")
        return [v.strip() for v in argv[index + 1].split(",") if v.strip()]
    return default


def run(lock: str, rows: list[str], threads: list[str]) -> tuple[str, list[str], Table]:
    """Builds and runs the benchmark with `lock`: the host line, the thread counts, and each row's cells."""
    env: dict[str, str] = dict(os.environ)
    env["RUSTFLAGS"] = f'{env.get("RUSTFLAGS", "")} --cfg lithia_sweep_lock="{lock}"'.strip()
    env["CARGO_TARGET_DIR"] = str(OUT / lock)
    env["LITHIA_BENCH"] = ",".join(rows)
    if threads:
        env["LITHIA_BENCH_THREADS"] = ",".join(threads)
    print(f"{lock}: building and running …", file=sys.stderr, flush=True)
    done: subprocess.CompletedProcess[str] = subprocess.run(
        ["cargo", "bench", "-p", "lithia-bench", "--bench", "scaling", "--locked"],
        cwd=ROOT, env=env, capture_output=True, encoding="utf-8", errors="replace",
    )
    (OUT / f"{lock}.log").write_text(done.stdout + done.stderr, encoding="utf-8", newline="\n")
    if done.returncode != 0:
        raise SystemExit(f"{lock}: the benchmark failed; see target/locks/{lock}.log")
    host: str = next((line for line in done.stdout.splitlines() if line.startswith("Host:")), "")
    counts: list[str] = []
    cells: Table = {}
    for line in done.stdout.splitlines():
        parts: list[str] = [p.strip() for p in line.strip().strip("|").split("|")]
        if line.startswith("| Benchmark |"):
            counts = parts[1:-1]
        elif len(parts) > 2 and parts[0] in rows:
            cells[parts[0]] = parts[1:-1]
    return host, counts, cells


def check() -> int:
    """Lints lithia built with each lock other than the default, so none falls behind the code."""
    failed: list[str] = []
    for lock in LOCKS[1:]:
        env: dict[str, str] = dict(os.environ)
        env["RUSTFLAGS"] = f'{env.get("RUSTFLAGS", "")} --cfg lithia_sweep_lock="{lock}"'.strip()
        env["CARGO_TARGET_DIR"] = str(OUT / "check")
        done: subprocess.CompletedProcess[str] = subprocess.run(
            ["cargo", "clippy", "-p", "lithate", "--locked", "--", "-D", "warnings"],
            cwd=ROOT, env=env, capture_output=True, encoding="utf-8", errors="replace",
        )
        print(f"{'ok' if done.returncode == 0 else 'FAIL'}  {lock}")
        if done.returncode != 0:
            print(done.stderr[-3000:])
            failed.append(lock)
    return 1 if failed else 0


def main(argv: list[str]) -> int:
    if isinstance(sys.stdout, io.TextIOWrapper):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")  # Windows consoles default to a code page
    if "-h" in argv or "--help" in argv:
        print(__doc__)
        return 0
    if argv == ["--check"]:
        return check()
    locks: list[str] = option(argv, "--locks", LOCKS)
    rows: list[str] = option(argv, "--rows", ROWS)
    threads: list[str] = option(argv, "--threads", [])
    unknown: list[str] = [lock for lock in locks if lock not in LOCKS]
    if unknown or not all(re.fullmatch(r"\d+", t) for t in threads):
        print(f"unknown locks or thread counts: {', '.join(unknown) or ', '.join(threads)}\n{__doc__}", file=sys.stderr)
        return 2
    OUT.mkdir(parents=True, exist_ok=True)
    results: dict[str, Table] = {}
    host: str = ""
    counts: list[str] = []
    for lock in locks:
        host, counts, results[lock] = run(lock, rows, threads)
    report: list[str] = [host, ""]
    for row in rows:
        report += [f"### {row}", "", f"| Lock | {' | '.join(counts)} |", f"|---|{'---|' * len(counts)}"]
        report += [f"| {lock} | {' | '.join(results[lock].get(row, ['?'] * len(counts)))} |" for lock in locks]
        report.append("")
    text: str = "\n".join(report)
    (OUT / "results.md").write_text(text, encoding="utf-8", newline="\n")
    print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
