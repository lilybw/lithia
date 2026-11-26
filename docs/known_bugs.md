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

- One process-wide lock serialises sweeps, rises of anchor counts from zero and writer registration.
  Benchmarks so far ran on two cores; contention on 8–64 cores is unmeasured.

## Gaps in verification

- The TLA+ models (`docs/tla`) check small graphs only, one feature or two per model, and leave out
  node creation and first runs in scheduling, keyed reclaiming and its reruns, async source blocks,
  runtime shutdown, `defer` and the rerun rate. `MCDiamond` has finished once in CI (26 147 949 states, depth 109, 3 h 57 min, no error); runners differ in speed,
  so CI stops it after 270 minutes and passes it if it found no error by then. The loom models (`crates/test/tests/loom/`) run with at
  most three preemptions on each push and four on nightly.
- cargo-mutants has run over the engine only as a local sample (29 mutants of `support.rs`); it
  runs over all of it weekly from v0.5.0.
