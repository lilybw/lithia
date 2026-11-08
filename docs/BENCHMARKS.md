# Benchmarks (spec §15)

Run with `cargo bench -p lithia-bench`. The numbers below come from a 2-core cloud container with a
shared CPU, in release mode, on 2026-11-08; they show orders of magnitude and relative costs only.
Re-run on your own machine before drawing conclusions.

| Benchmark | Result | Informs |
|---|---|---|
| Spawn overhead | 4.16 µs per run spawned vs 1 ns per body inline | Whether the fast path (11.4 step 4) belongs in v0 |
| Framing | fan-out of 1000: 3.11 ms per write; 100 writes: 95 µs batched in a lithia task vs 203 µs one batch per write | Whether framing needs caching, and batching advice (6.2) |
| Fan-in contention | 12 800 writes into a 64-parent join: 9.28 µs per write, join ran 2 303 times | The lock design (11.4 step 1) |
| Clock contention | 1 writer: 1.14 µs per write; 2 writers: 1.17 µs; 4 writers: 1.32 µs | Falling back to one seen version per parent (5, 7.2) |
| Protocol overhead | 2.02 µs per node run vs ~1 ns per node recomputed under one global lock | Whether lithia's subset of MV-RP is as cheap as expected (11.4) |
| Writer sets in meshes | n=2: 2 runs; n=3: 3; n=4: 4; n=6: 7; n=8: 10 (all well under n²) | Whether accumulation needs tuning (7.4) |
| Drop churn | 100 per-item effects recreated: 2.17 ms per rerun | Batching drops (10.2, 10.3) |
| Rerun rates | one node driven as fast as `flush` allows: ~45 000 runs/s | The default threshold for the rerun-rate warning (12, 17) |

## Observations

- **Spawn overhead dominates.** Every run is a tokio task (v0 leaves the fast path out). A cheap body
  costs ~4 µs to schedule against ~1 ns to call, so the inline fast path is the biggest available win.
- **The protocol-overhead comparison is lopsided by design.** The global-lock baseline recomputes the
  graph in a loop with no scheduling at all; the gap is mostly spawn overhead, not framing or writer sets.
- **The rerun-rate default is too low.** A healthy node driven as fast as possible reruns ~45 000
  times a second, far above the provisional 1 000/s default, so the warning would fire on legitimate
  fast input. The threshold is configurable through `LITHIA_RERUN_RATE`; the default needs deciding (§17).
- **Batching pays off.** 100 writes cost about half as much as one batch as they do as 100 batches.
- **Clock contention is mild** at this scale: 4 parallel writers cost ~15% more per write than one.
