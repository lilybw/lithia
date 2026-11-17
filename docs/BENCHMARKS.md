# Benchmarks (spec §15)

Run with `cargo bench -p lithia-bench`. The numbers below come from a 2-core cloud container with a
shared CPU, in release mode: three runs of v0.3.0 on 2026-11-09 and 2026-11-11, given as a range where they
differ. Two runs of v0.2.5, alternated with the last two, fell in the same ranges except where noted. They
show orders of magnitude and relative costs only; re-run on your own machine before drawing conclusions.

| Benchmark | Result | Informs |
|---|---|---|
| Spawn overhead | 4.2–4.5 µs per run spawned vs 1–2 ns per body inline | Whether the fast path (11.4 step 4) belongs in v0 |
| Framing | fan-out of 1000: 3.1–4.1 ms per write; 100 writes: 77–189 µs batched in a lithia task vs 179–187 µs one batch per write | Whether framing needs caching, and batching advice (6.2) |
| Fan-in contention | 12 800 writes into a 64-parent join: 7.5–15 µs per write, join ran 2 300–2 500 times | The lock design (11.4 step 1) |
| Clock contention | 1 writer: 1.5–1.7 µs per write; 2 writers: 1.6–2.0 µs; 4 writers: 1.6–1.9 µs | Falling back to one seen version per parent (5, 7.2) |
| Protocol overhead | 2.0–2.8 µs per node run vs ~1 ns per node recomputed under one global lock | Whether lithia's subset of MV-RP is as cheap as expected (11.4) |
| Writer sets in meshes | n=2: 2 runs; n=3: 3; n=4: 4; n=6: 6–8; n=8: 13–15 (v0.2.5: 8; all well under n²) | Whether accumulation needs tuning (7.4) |
| Drop churn | 100 per-item effects recreated: 2.1–2.5 ms per rerun | Batching drops (10.2, 10.3) |
| Rerun rates | one node driven as fast as `flush` allows: 41 000–50 000 runs/s | The default threshold for the rerun-rate warning (12, 17) |
| Handles (v0.3.0) | clone and drop 19 ns with the anchoring count vs 11 ns for a bare `Arc`; a read 20–21 ns | Whether the per-node token design is needed (10.1) |
| Creation (v0.3.0) | a bound effect 0.8–1.0 µs, one held by support 0.9–1.7 µs | The cost of support at creation (10.1) |
| Sweep over a hub (v0.3.0) | letting go of and freeing 10 000 handle-less watchers: 219–294 ms, vs 216–284 ms to free as many scoped watchers by their handles | The sweep's share of a large let-go (10.1) |
| Rebuilding children (v0.3.0) | 100 children rebuilt: 2.8–3.5 ms per rerun | Generations against value-held children (10.1) |
| Last-handle drops (v0.3.0) | the `helper(&x.accessor())` idiom: 250–570 ns per drop on one thread, 1.0–3.7 µs on each of two | The 1 µs drop budget and the one sweep lock (10.1, 11.4) |
| Keyed rows (v0.3.1) | 100 rows per list rerun, rebuilt against `key = row.id`, median of ten runs: no row changed 2.73 ms vs 78 µs; one changed 2.76 ms vs 90 µs; reversed 2.86 ms vs 81 µs; all changed 2.77 ms vs 2.34 ms | What reclaiming by key saves (10.1, Keyed nodes) |

## Observations

- **Spawn overhead dominates.** Every run is a tokio task (v0 leaves the fast path out). A cheap body
  costs ~4 µs to schedule against ~1 ns to call, so the inline fast path is the biggest available win.
- **The protocol-overhead comparison is lopsided by design.** The global-lock baseline recomputes the
  graph in a loop with no scheduling at all; the gap is mostly spawn overhead, not framing or writer sets.
- **The rerun-rate default is too low.** A healthy node driven as fast as possible reruns 41 000–50 000
  times a second, far above the provisional 1 000/s default, so the warning would fire on legitimate
  fast input. The threshold is configurable through `LITHIA_RERUN_RATE`; the default needs deciding (§17).
- **Batching pays off.** 100 writes cost about half as much as one batch as they do as 100 batches.
- **Clock contention is mild** at this scale: 4 parallel writers cost at most ~25% more per write than one.
- **Lifetimes are cheap where it matters.** An anchoring count adds ~8 ns to a handle clone and drop, a
  temporary anchor on a supported node costs well under the 1 µs budget, and a sweep's share of a large
  let-go is lost in the cost of freeing. Concurrent drops contend on the one sweep lock (spec §16).
- **The eight-way mesh** took 13–15 runs under v0.3.0 against 8 under v0.2.5, run alternately; nothing in
  v0.3.0 changes propagation, so this is an open question (spec §17).
- **Keys pay off when few rows change.** A list rerun that keeps its rows is 30 to 35 times cheaper keyed,
  reordered or not: a reclaim with equal captures costs under 1 µs against ~27 µs to rebuild a row. When
  every row changes, refreshing in place is still slightly cheaper than rebuilding.
- **v0.3.1 against v0.3.0** (2026-11-17, ten alternated runs each): every median of v0.3.1's common
  benchmarks falls within v0.3.0's range, so unkeyed nodes pay nothing for keys. That container ran faster
  than the one behind the table above, so compare within a session only.
