#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 lithia Authors
# SPDX-License-Identifier: MPL-2.0

"""Applies each hand-picked mutation to a copy of the workspace and checks that the test named for it
fails (docs/VERIFICATION.md, Mutations). The working tree is never changed.

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
        "W1", "A parent framed again before its release arrived counted once", "crates/lithia/src/protocol/propagate.rs",
        "                    entry.waiting.add(index);\n                    entry.due.remove(Due::RUNNABLE);",
        "                    entry.waiting.ensure(index);\n                    entry.due.remove(Due::RUNNABLE);",
        "loom", "two_runs_write_one_source_in_one_epoch",
    ),
    Mutation(
        "W2", "A superseded run's published first item not carried to its successor", "crates/lithia/src/protocol/lifecycle.rs",
        "            carry_published(node, shared, own_stamp);\n",
        "            let _ = (node, shared, own_stamp);\n",
        "loom", "a_change_published_before_a_supersede_reaches_the_children",
    ),
    Mutation(
        "W3", "A written node's release reaching its subscribers at release time, not those it framed "
        "(equivalent: a child linked in between waits on the written node only through an entry, run or "
        "handover that releases it too; the early release reads the same value, and the later one is ignored)",
        "crates/lithia/src/protocol/deliver.rs",
        "        for (node, stamp) in &writes {\n            let children: Children = counted.next().unwrap_or_default();\n",
        "        for (node, stamp) in &writes {\n            let _ = counted.next();\n"
        "            let children: Children = node.header().subscribers();\n",
        "edge", "", caught=False,
    ),
    Mutation(
        "W4", "A `batches = WHOLE` node never running past its bound", "crates/lithia/src/protocol/propagate.rs",
        "        if sched.whole_deferred >= WHOLE_EPOCHS_BOUND {\n",
        "        if sched.whole_deferred >= u32::MAX {\n",
        "edge", "a_whole_batches_join_runs_once_input_has_kept_it_waiting_too_long",
    ),
    Mutation(
        "F1", "The fast path keeping runs of another runtime", "crates/lithia/src/exec/fast_path.rs",
        "    open.is_some() && open == node\n",
        "    open.is_some() || open == node\n",
        "edge", "bodies_run_on_their_own_runtime_on_the_fast_path_too",
    ),
    Mutation(
        "F2", "Runs left past a task's budget dropped instead of handed on", "crates/lithia/src/exec/fast_path.rs",
        "    left.extend(queue.drain(..));\n",
        "    queue.clear();\n",
        "edge", "a_wide_fan_out_runs_every_child_once_per_write",
    ),
    Mutation(
        "F3", "A runner task dropped before its poll dropping its runs", "crates/lithia/src/exec/fast_path.rs",
        "        for weak in self.queue.drain(..) {\n            if let Some(node) = weak.upgrade() {\n"
        "                node.spawn_run();\n            }\n        }\n",
        "        self.queue.clear();\n",
        "edge", "ready_runs_packed_for_a_runtime_that_shuts_down_fail_like_runs",
    ),
    Mutation(
        "F4", "Tokens of one epoch closed together counted as one", "crates/lithia/src/graph/epoch.rs",
        "            epoch.close_tokens(count as usize);\n",
        "            epoch.close_tokens(1);\n",
        "deep_chain", "",
    ),
    Mutation(
        "F5", "The share a task keeps packed into a runner task as well (equivalent: still runs once)",
        "crates/lithia/src/exec/fast_path.rs",
        "    if keep(&nodes[..share]) {\n        nodes.drain(..share);\n    }\n",
        "    let _ = keep(&nodes[..share]);\n",
        "edge", "", caught=False,
    ),
    Mutation(
        "S1", "A walk from a scoped or held anchor stopping at it", "crates/lithia/src/lifetime/support.rs",
        "        if !stop || header.id == start {\n",
        "        if !stop {\n",
        "edge", "a_scoped_node_that_is_an_anchor_again_holds_what_depends_on_it",
    ),
    Mutation(
        "S2", "A new writer holding nothing again", "crates/lithia/src/lifetime/support.rs",
        "            rehold(node.clone(), &mut keep, &mut held);\n",
        "            let _ = &mut held;\n",
        "edge", "a_new_writer_holds_again_what_support_let_go_of",
    ),
    Mutation(
        "S3", "A rise from zero never walking", "crates/lithia/src/lifetime/support.rs",
        "    cfg!(lithia_loom) || LET_GO.load(Ordering::SeqCst) != 0\n",
        "    cfg!(lithia_loom)\n",
        "edge", "an_anchor_that_returns_holds_again_what_support_let_go_of",
    ),
    Mutation(
        "K1", "A write stopping a block without waking it", "crates/lithia/src/protocol/lifecycle.rs",
        "        diag::Diag::Superseded { site: node.header().site, run: shared.id() }.emit();\n        shared.wake();\n",
        "        diag::Diag::Superseded { site: node.header().site, run: shared.id() }.emit();\n",
        "loom", "a_write_wakes_the_parked_block_it_stops",
    ),
    Mutation(
        "K2", "Disposal retiring a run without waking it", "crates/lithia/src/protocol/lifecycle.rs",
        "    if let Some(shared) = shared {\n        shared.cancel();\n        shared.wake();\n    }\n    drain(work);\n",
        "    if let Some(shared) = shared {\n        shared.cancel();\n    }\n    drain(work);\n",
        "loom", "disposal_wakes_a_parked_run",
    ),
    Mutation(
        "K3", "A freed node cancelling its run without waking it", "crates/lithia/src/graph/node.rs",
        "            slot.shared.cancel();\n            slot.shared.wake();\n",
        "            slot.shared.cancel();\n",
        "loom", "freeing_a_node_wakes_its_parked_run",
    ),
    Mutation(
        "W5", "A run failing after its release telling no child", "crates/lithia/src/protocol/lifecycle.rs",
        "    let tells: bool = failed && !first;\n",
        "    let tells: bool = false;\n",
        "edge", "a_failure_after_the_first_suspension_reaches_strict_children",
    ),
    Mutation(
        "W6", "An absorbed change settling beside a superseded run's handover", "crates/lithia/src/protocol/propagate.rs",
        "        None => &sched.handover,\n",
        "        None => return false,\n",
        "edge", "flush_waits_for_the_successor_of_the_run_that_absorbed_its_write",
    ),
    Mutation(
        "W7", "A framing walk opening entries below a ghost", "crates/lithia/src/protocol/propagate.rs",
        "                Some(_) if header.is_ghost() || parent.is_ghost() => None,\n",
        "                Some(_) if header.is_ghost() => None,\n",
        "loom", "a_write_frames_through_a_node_being_disposed",
    ),
    Mutation(
        "W8", "A shutdown not telling the children of the dropped run's epochs", "crates/lithia/src/protocol/lifecycle.rs",
        "    if !ghost && !served.is_empty() {\n",
        "    if false && !served.is_empty() {\n",
        "edge", "a_runtime_shutting_down_fails_the_strict_children_of_its_runs",
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
