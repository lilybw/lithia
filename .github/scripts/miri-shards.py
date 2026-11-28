#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 lithia Authors
# SPDX-License-Identifier: MPL-2.0

"""Prints the Miri job matrix as JSON, read from the test files so that no new test goes unchecked:
the unit tests and validation cases in one shard, then the edge tests' modules in `SHARDS`, by size.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

ROOT: Path = Path(__file__).resolve().parents[2]
SHARDS: int = 3
MIRI: str = "cargo miri test --locked --no-fail-fast"
# trybuild runs rustc, which Miri cannot.
NOT_UNDER_MIRI: set[str] = {"compile"}


def edge_shards() -> list[list[str]]:
    """The edge modules, largest first onto the lightest shard, each shard in name order."""
    edge: Path = ROOT / "crates/test/tests/edge"
    modules: list[str] = re.findall(r"^mod (\w+);", (edge / "main.rs").read_text(), re.MULTILINE)
    sizes: dict[str, int] = {name: len((edge / f"{name}.rs").read_text().splitlines()) for name in modules}
    shards: list[list[str]] = [[] for _ in range(SHARDS)]
    weights: list[int] = [0] * SHARDS
    for name in sorted(modules, key=lambda name: (-sizes[name], name)):
        lightest: int = weights.index(min(weights))
        shards[lightest].append(name)
        weights[lightest] += sizes[name]
    return [sorted(shard) for shard in shards if shard]


def main() -> None:
    validation: list[str] = sorted(
        path.stem for path in (ROOT / "crates/validation/tests").glob("*.rs") if path.stem not in NOT_UNDER_MIRI
    )
    tests: str = " ".join(f"--test {name}" for name in validation)
    shards: list[dict[str, str]] = [{
        "name": "unit tests and validation cases",
        "run": f"{MIRI} -p lithate --lib && {MIRI} -p lithia-validation {tests}",
    }]
    for number, modules in enumerate(edge_shards(), start=1):
        filters: str = " ".join(f"{name}::" for name in modules)
        shards.append({"name": f"edge tests {number}", "run": f"{MIRI} -p lithia-test --test edge -- {filters}"})
    print(json.dumps(shards))


if __name__ == "__main__":
    main()
