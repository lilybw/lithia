<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# Known bugs and limitations

Everything known to be wrong or missing in lithia, most frequent first. A frequency is what has been
seen, or what follows from the design where nothing has been seen. Fixed bugs are in
[`CHANGELOG.md`](../CHANGELOG.md).

## Reproduced, not yet fixed

### An epoch settling before the run that absorbed its change publishes

*Every time a run, started for one epoch, reads a write whose own epoch reaches the node only later.
Reproduced deterministically by `flush_waits_for_the_run_that_absorbed_its_write`
(`crates/test/tests/edge/propagation.rs`, ignored until fixed).*

A node runs for an epoch only if a release in it carries a change newer than the start of its last run
(spec §7.2). When a run has started after a write but before that write's epoch reaches the node, the
write's entry settles unchanged, although the run that read the write has not published yet. The
write's epoch can then settle: `flush` returns, and `defer` runs, while the node still holds its old
value. Found by the architectural review of 2026-11-22. It very likely explains the macOS failures of
`defer_outside_lithia_tasks_follows_the_latest_write`, where `seen` was still 0 after `flush`: the
node's first run, started after the write, had not published. Stress runs report that test's failures
without failing (`.github/scripts/known-flaky.txt`) until the fix lands.

The fix: an entry that would settle because a run in flight absorbed its change joins that run, which
then releases the entry's epoch when it publishes. If the run has already released, the entry settles
with the node's current version, so its children test the change in turn. `Scheduling.tla` misses this
because it has one scope and `ScopeConsistent` checks only that runs started; it needs two scopes and the
property "an epoch settles only after every run it reached has published".

### `effect!` turns some valid bodies into compile errors

*Every time, for the forms below.*

A constant or static whose name starts with an underscore, such as `const _LIMIT`, is taken for a
lowercase binding, captured, and then rejected (E0308). The code review of 2026-11-22 found more forms
that fail the same way: a `use` inside a body, bindings made by a statement macro such as
`tokio::pin!`, and `"{name}"` inside string literals that are not format strings. It also found that
the capture analysis sees nothing inside a macro whose arguments it cannot parse, such as tracing's
`%value` fields. The fixes are listed in the review.

## Seen in testing

### Case BV timing out

*2 in about 2 100 runs on macOS ARM CI runners since the test's rework, both in one job; never in
7 300 runs on Linux, under load or pinned to one core.*

`case_bv_whole_epochs` (`crates/validation/tests/consistency.rs`) timed out after 20 seconds. The test
blocks one of its two worker threads in a synchronous body on purpose, and whether the engine or that
blocked worker stalls the rest is not known. Not investigated further at this frequency; listed in
`known-flaky.txt`.

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
  `WHOLE_EPOCHS`, keyed nodes, disposal and ghosts, and check small graphs only. `MCDiamond` has
  finished once in CI (26 147 949 states, depth 109, 3 h 57 min, no error); runners differ in speed,
  so CI stops it after 270 minutes and passes it if it found no error by then. The loom models (`crates/test/tests/loom.rs`) run with at
  most three preemptions on each push and four on nightly.
- cargo-mutants has run over the engine only as a local sample (29 mutants of `support.rs`); it
  runs over all of it weekly from v0.5.0.
