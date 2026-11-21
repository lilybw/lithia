#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 lithia Authors
# SPDX-License-Identifier: MPL-2.0

"""Applies each hand-picked mutation to a copy of the workspace and checks that the test named for it
fails (spec section 11.4, "Teeth"). The working tree is never changed.

Usage: mutations.py [--list] [ID ...]. Exits 1 if a mutation that should be caught is not.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

ROOT: Path = Path(__file__).resolve().parents[2]
LOOM: dict[str, str] = {"RUSTFLAGS": "--cfg lithia_loom", "LOOM_MAX_PREEMPTIONS": "3"}


@dataclass(frozen=True)
class Mutation:
    id: str
    what: str
    file: str
    old: str
    new: str
    # The test that must fail: (cargo test arguments, name filter); loom tests run under the loom cfg.
    suite: str
    test: str
    caught: bool = True


MUTATIONS: list[Mutation] = [
    Mutation(
        "W1", "A parent framed again before its release arrived counted once", "crates/lithia/src/protocol.rs",
        "                        entry.waiting.add(index);\n                        entry.runnable = false;",
        "                        entry.waiting.ensure(index);\n                        entry.runnable = false;",
        "loom", "two_runs_write_one_source_in_one_epoch",
    ),
    Mutation(
        "W2", "A superseded run's published first item not carried to its successor", "crates/lithia/src/protocol.rs",
        "            carry_published(node, shared, own_stamp);\n",
        "            let _ = (node, shared, own_stamp);\n",
        "loom", "a_change_published_before_a_supersede_reaches_the_children",
    ),
    Mutation(
        "W3", "A written node's release reaching its subscribers at release time, not those it framed "
        "(uncaught: no test creates a node between a write's framing and its release)",
        "crates/lithia/src/protocol.rs",
        "        for (node, stamp) in &writes {\n            let children: Vec<NodeRef> = counted.next().unwrap_or_default();\n",
        "        for (node, stamp) in &writes {\n            let _ = counted.next();\n"
        "            let children: Vec<NodeRef> = node.header().subscribers();\n",
        "edge", "", caught=False,
    ),
    Mutation(
        "W4", "A WHOLE_EPOCHS node never running past its bound", "crates/lithia/src/protocol.rs",
        "        if sched.whole_deferred >= WHOLE_EPOCHS_BOUND {\n",
        "        if sched.whole_deferred >= u32::MAX {\n",
        "edge", "a_whole_epochs_join_runs_once_input_has_kept_it_waiting_too_long",
    ),
    Mutation(
        "F1", "The fast path keeping runs of another runtime", "crates/lithia/src/inline.rs",
        "    open.is_some() && open == node\n",
        "    open.is_some() || open == node\n",
        "edge", "bodies_run_on_their_own_runtime_on_the_fast_path_too",
    ),
    Mutation(
        "F2", "Runs left past a task's budget dropped instead of handed on", "crates/lithia/src/inline.rs",
        "    left.extend(queue.drain(..));\n",
        "    queue.clear();\n",
        "edge", "a_wide_fan_out_runs_every_child_once_per_write",
    ),
    Mutation(
        "F3", "A runner task dropped before its poll dropping its runs", "crates/lithia/src/inline.rs",
        "        for weak in self.queue.drain(..) {\n            if let Some(node) = weak.upgrade() {\n"
        "                node.spawn_run();\n            }\n        }\n",
        "        self.queue.clear();\n",
        "edge", "ready_runs_packed_for_a_runtime_that_shuts_down_fail_like_runs",
    ),
    Mutation(
        "F4", "Tokens of one epoch closed together counted as one", "crates/lithia/src/protocol.rs",
        "        epochs[at].close_tokens(same);\n",
        "        epochs[at].close_tokens(1);\n",
        "deep_chain", "",
    ),
    Mutation(
        "F5", "The share a task keeps packed into a runner task as well (equivalent: still runs once)",
        "crates/lithia/src/inline.rs",
        "    if keep(&nodes[..share]) {\n        nodes.drain(..share);\n    }\n",
        "    let _ = keep(&nodes[..share]);\n",
        "edge", "", caught=False,
    ),
]

SUITES: dict[str, list[str]] = {
    "edge": ["-p", "lithia-test", "--test", "edge"],
    "deep_chain": ["-p", "lithia-test", "--test", "deep_chain"],
    "loom": ["-p", "lithia-test", "--test", "loom", "--release"],
}


def run(mutation: Mutation, copy: Path, target: Path) -> str:
    """Applies `mutation` in `copy`, runs its suite, restores the file; returns the outcome."""
    path: Path = copy / mutation.file
    original: str = path.read_text()
    if original.count(mutation.old) != 1:
        return "STALE: the code to mutate is not found exactly once"
    path.write_text(original.replace(mutation.old, mutation.new))
    try:
        loom: bool = mutation.suite == "loom"
        env: dict[str, str] = dict(os.environ)
        env["CARGO_TARGET_DIR"] = str(target / ("loom" if loom else "test"))
        if loom:
            env.update(LOOM)
        args: list[str] = ["cargo", "test", "--locked", *SUITES[mutation.suite], "--", mutation.test]
        try:
            proc: subprocess.CompletedProcess[str] = subprocess.run(
                args, cwd=copy, env=env, capture_output=True, text=True, timeout=3600
            )
        except subprocess.TimeoutExpired:
            return "caught (timed out)"
        out: str = proc.stdout + proc.stderr
        if "could not compile" in out:
            return "COMPILE ERROR"
        failed: bool = proc.returncode != 0 and ("FAILED" in out or "panicked" in out)
        return "caught" if failed else "not caught" if proc.returncode == 0 else f"ERROR (exit {proc.returncode})"
    finally:
        path.write_text(original)


def main(argv: list[str]) -> int:
    if "--list" in argv:
        for mutation in MUTATIONS:
            print(f"{mutation.id}  {mutation.what}")
        return 0
    chosen: list[Mutation] = [m for m in MUTATIONS if not argv or m.id in argv]
    failed: bool = False
    with tempfile.TemporaryDirectory(prefix="lithia-mutations-") as tmp:
        copy: Path = Path(tmp) / "src"
        ignore: Callable[[str, list[str]], set[str]] = shutil.ignore_patterns("target", ".git")
        shutil.copytree(ROOT, copy, ignore=ignore)
        target: Path = Path(os.environ.get("LITHIA_MUTATIONS_TARGET", str(ROOT / "target" / "mutations")))
        for mutation in chosen:
            outcome: str = run(mutation, copy, target)
            expected: str = "caught" if mutation.caught else "not caught"
            ok: bool = outcome == expected
            failed |= not ok
            print(f"{'ok  ' if ok else 'FAIL'} {mutation.id} {outcome}: {mutation.what}", flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
