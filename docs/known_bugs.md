<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# Known bugs and limitations

Everything known to be wrong or missing in lithia, most frequent first. A frequency is what has been
seen, or what follows from the design where nothing has been seen. Fixed bugs are in
[`CHANGELOG.md`](../CHANGELOG.md).

## Seen in testing

### `flush` returning before a deferred function runs

*About 1 in 450 runs of one test on macOS ARM CI runners (7 in about 3 100); never in 3 000 runs on
Linux x86-64 or ARM.*

`defer_outside_lithia_tasks_follows_the_latest_write` (`crates/test/tests/edge/propagation.rs`) defers a
function from a plain tokio runtime's `block_on`, outside any lithia task, then awaits `lithia::flush`;
on the failing runs `flush` returned before the function had run. Only `defer` called outside lithia
tasks is affected. Not investigated further at this frequency; stress runs report its failures
without failing (`.github/scripts/known-flaky.txt`).

## Never seen; follow from the design

### Writer sets confusing two lineages

*Two of a million nodes created in runs and alive at once share a lineage with probability about
5 × 10⁻⁸.*

A node created in a run writes under its lineage, a 63-bit hash of its creator, site and place in the
run (spec §7.4), so the same creation in the creator's next run keeps the identity. Two creations whose
lineages collide look like one writer: a write by one can be cut, as though it fed back into the other,
and that write's rerun is lost. Lineages stay hashed rather than interned, by decision (spec §17).

### `WHOLE_EPOCHS` running before every epoch is whole

*Only under input that never pauses: 64 epochs become ready while the node waits for another.*

A `WHOLE_EPOCHS` node starts no run while an epoch that reached it is still on its way. So that input
without pauses cannot hold it back for ever, once 64 epochs have become ready while it waits it runs on
them as a node without the flag would, and warns once per site (spec §8, §12).

## Leaks and duplicates by design

Each is described in the spec, with the way to avoid it; `lithia::test::live_nodes` and the growth
warning catch the leaks.

- Self-support: a top-level node whose value, or an `Arc` its body captured, holds an outside handle to
  a node upstream of it supports itself until it is scoped or disposed (§10.1, Case AW).
- Value cycles: a source node whose value holds a handle to one of its own subscribers is never freed
  (§10.1, Case AF).
- Overlapping generations: effects held in a replaced value react, beside their replacements, until the
  value's drop has run (§10.2).

## Performance

- While any node is scoped, every sweep walks handleless parents up to an anchor, so building a deep
  chain of handleless nodes takes quadratic time. A count of live scoped nodes, or a walk forward from
  them, would remove it.
- One process-wide lock serialises sweeps, rises of anchor counts from zero and writer registration.
  Benchmarks so far ran on two cores; contention on 8–64 cores is unmeasured.

## Gaps in verification

- Two kept mutations survive the tests (`.github/scripts/mutations.py`): W3, a written node's release
  reaching its subscribers at release time rather than those it framed, needs a node created between a
  write's framing and its release; F5 is equivalent to the original.
- The TLA+ models (`docs/tla`) leave out creator order, fragility, reflow, generators,
  `WHOLE_EPOCHS`, keyed nodes, disposal and ghosts, and check small graphs only; `MCDiamond` only up
  to the depth TLC reaches in 270 minutes. The loom models (`crates/test/tests/loom.rs`) run with at
  most three preemptions on each push and four on nightly.
- cargo-mutants has run over the engine only as a local sample (29 mutants of `support.rs`); it
  runs over all of it weekly from v0.5.0.
