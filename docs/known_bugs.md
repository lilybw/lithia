<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# Known bugs and limitations

Everything known to be wrong or missing in lithia, most frequent first. A frequency is what has been
seen, or what follows from the design where nothing has been seen. Fixed bugs are in
[`CHANGELOG.md`](../CHANGELOG.md). What the design chooses is stated in the spec instead: the
64-epoch bound on a `batches = WHOLE` node's wait (§7.2), and the nodes that self-support, a value
cycle or overlapping generations keep alive until they are scoped, disposed or dropped (§10.1, §10.2).

## Reproduced, not yet fixed

None.

## Seen in testing

None.

## Performance

- One process-wide lock serialises sweeps, rises of anchor counts from zero and writer registration,
  and it contends: on a 16-core Ryzen 9 7950X3D, a last-handle drop takes 311 ns on one thread and
  91 µs on each of 32 at once (`docs/BENCHMARKS.md`). The version clock, by contrast, scales.

## Gaps in verification

- The TLA+ models (`docs/tla`) check small graphs only, one feature or two per model, and leave out
  node creation and first runs in scheduling, keyed reclaiming and its reruns, async source blocks,
  runtime shutdown, `defer` and the rerun rate. `MCDiamond` has finished once in CI (26 147 949 states, depth 109, 3 h 57 min, no error); runners differ in speed,
  so CI stops it after 270 minutes and passes it if it found no error by then. The loom models (`crates/test/tests/loom/`) run with at
  most three preemptions on each push and four on nightly.
- cargo-mutants makes 1 640 mutants of the engine. `.cargo/mutants.toml` leaves out 169 that are
  compiled only for loom or cannot change what lithia does (`VERIFICATION.md` names the groups). Of
  the rest, 160 survive the tests (review of 2026-11-27). Most are protocol and lifetime paths that
  only particular interleavings reach: supersession with handovers, cut reruns, creator gating and its
  two resume paths, children linked while a parent runs or is disposed, sweeps deferred to an
  operation's end, `settled`'s quiet checks between steps. The TLA+ and loom models check those paths;
  cargo-mutants runs neither. Of 11 such mutants run against the loom models at two preemptions, 5
  fail a model. The others no test tells apart: `LITHIA_RERUN_RATE` and
  `LITHIA_TEST_TIMEOUT_MS` set to 0, a lineage's copy of a key or a keyed node's kept inputs dropped
  inline, place counts of unkeyed creations at one site, drops grouped by runtime, and parents released
  without the worklist (the 20 002-node chain frees without recursion either way).
