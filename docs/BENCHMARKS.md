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
- **The rerun-rate default.** A healthy node driven as fast as possible reruns 41 000–50 000 times a
  second, and 120 000–140 000 on the fast path, far above the first default of 1 000/s. Since v0.5.0 the
  default is 50 000/s; `LITHIA_RERUN_RATE` overrides it.
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

## v0.4.0 against v0.3.1 (2026-11-20)

Runs alternated in the same 2-core container: two of v0.3.1 (`release`, 38899ee), two of v0.4.0 as first
built, and three after the fixes below.

- Every benchmark falls within v0.3.1's range, keyed rows included (70–90 µs per unchanged list rerun,
  against 72–90 µs), except two:
  - Last-handle drops on a supported node: 605–641 ns, against 499–528 ns. The temporary anchor's arrival
    walks downstream to re-hold what support let go of (spec §10.1), which stops at once at a held node.
    Still within the 1 µs budget.
  - Writer sets in meshes run more, since cut writers rerun once their epoch settles (spec §7.4):
    n=2: 3 runs; 3: 5; 4: 7; 6: 11; 8: 15–22 (v0.3.1: 2, 3, 4–5, 7–10, 9–16), all far below n².
- Found by the first v0.4.0 runs and removed: `arc_swap` cells in every handle (lease) and node (lease,
  comparator), whose drop scans every thread's debt slots, so a handle's clone and drop took 1.2 µs (now
  20 ns, as before); a full delivery for every reclaimed keyed node (keyed rows took 350–440 µs); an `Arc`
  clone on every anchoring handle (handles took 32–37 ns).

## v0.4.0 performance pass (2026-11-20)

After a code review. Five runs alternated with five of v0.4.0 as first committed (c5b1216), medians. New
rows: `Discarded values` and `Wide creation` in `benches/spec.rs`; `benches/allocs.rs` counts heap
allocations with a counting allocator on a one-thread runtime. `LITHIA_BENCH=spawn,clock` runs only the
named rows.

| Benchmark | Before | After |
|---|---|---|
| Spawn overhead | 4.8 µs per run | 3.9 µs |
| Protocol overhead | 1.8 µs per node run | 1.25 µs |
| Framing | 100 writes: 57 µs batched, 172 µs one batch per write | 35 µs, 123 µs; fan-out unchanged (10 framing-only alternations: +1%) |
| Fan-in contention | 5.9 µs per write | 3.8 µs |
| Clock contention | 0.90 / 1.10 / 1.05 µs per write (1 / 2 / 4 writers) | 0.60 / 0.64 / 0.59 µs |
| Drop churn | 1.86 ms per rerun | 0.60 ms |
| Rerun rates | 77 000 runs/s | 126 000 runs/s |
| Creation and sweeps | bound effect 1.0 µs; sweep freeing 10 000 watchers 177 ms | 0.86 µs; 147 ms |
| Rebuilding children | 3.3 ms per rerun | 0.65 ms |
| Keyed rows | rebuilt 2.7–2.9 ms; keyed, all changed, 2.2 ms | 0.69–0.75 ms; 0.60 ms; keyed with few changes unchanged (73–82 µs) |
| Discarded values | equal run 18 µs; equal write 9.2 µs; `String` write 12.5 µs | 8.4 µs; 57 ns; 260 ns |
| Wide creation | 10 000 children per run: 315 ms per rerun | 77 ms |
| Allocations | chain 17 per run; fan-out 14.6 per child run; detached write 25; creation 24 per node | 10; 7.5; 18; 17 |

- Bug: values `equals` or a superseded run discarded skipped the `needs_drop` check (spec §10.3), so an
  equal `u64` write cost a `spawn_blocking`. Strings and byte buffers now also drop inline, as §10.3 says.
- Drops are gathered per protocol step, run poll and dropped parcel into one blocking task per runtime
  (at most 64 payloads each): most of drop churn, rebuilding and keyed rebuilds was `spawn_blocking`.
- `Header::unsubscribe` scanned the parent's subscriber `Vec`, so freeing n children of one node was
  O(n²) (31% of instructions at 2 000 children in callgrind); subscribers are now a `BTreeMap` by id.
  The creator's `created` list is pruned when it doubles.
- Allocation diet: `ParentMask` keeps 64 bits inline; `Sched::entries` is a sorted `Vec` instead of a
  `BTreeMap` (whose first insert keeps a ~1 KB leaf per framed node); empty polls skip the served-epoch
  copy and `deliver_into`; token lists are moved, not extended; `start_run` takes ready entries in place.
- Locks: the run waker is set only for futures; `failure` is swapped only when `FAILED` was set, and a
  run start updates the phase in one step; writes to plain source nodes skip `kill_run`; `changes()`
  makes its channel on first use; detached writes cache their task's scope per thread.
- Meshes vary with scheduling in both: n=8 over 15 runs each took 15–42 runs before, 18–58 after
  (medians 21–23 and 25–26), all below n² = 64.
- Left out: merging a run's sched-lock round trips (reorders protocol steps for two uncontended locks;
  deprioritized); pooled scratch buffers and `RunShared` reuse (seven of the ten allocations left per
  run, about 4% of a run; planned for v0.4.1); writer sets kept inline (no allocation on runs that
  write nothing); striped support counters (top-level creation only, beside the one sweep lock); a
  slimmer node record (wide diff, unmeasured); reused sweep buffers (a sweep usually asks once); a
  coarse rerun clock (a vDSO call on Linux). The fast path, runner tasks and padding follow below.

## v0.4.0 fast path (2026-11-20)

`crates/lithia/src/inline.rs`, spec §11.4 step 4. A run or runner task keeps the ready synchronous runs
of its own runtime that its steps hand on (up to 16 queued) and polls each as its own `RunFuture` in a
loop, up to 32 runs or 50 µs; then cheap leftovers go on in one runner task and, after a heavy run, each
gets its own task. A step readying several synchronous runs of one runtime packs them into runner tasks,
`ceil(n / workers)` clamped to 1..=16 each, the dispatching task keeping the first share when its fast
path is open for that runtime. Async runs and generators keep their own tasks. Fast-path runs close their
tokens when the task's poll ends, coalesced per epoch (`Epoch::close_tokens`). Hot statics (`CLOCK`,
`NEXT_NODE`, `NEXT_EPOCH`, `WAITERS`, `QUEUED`, `SWEEP`, `ANY_SCOPED`) are 128-byte aligned: `nm` showed
the clock and both id counters on one line, and the settle signal, sweep lock and drop count on another.

Five runs alternated with five of 60b5ba8 (the performance pass), medians:

| Benchmark | Before | After |
|---|---|---|
| Spawn overhead (chain) | 3.9 µs per run | 1.25 µs |
| Framing, 1 000-wide fan-out | 4.0 ms per write | 1.2 ms |
| Fan-in contention | 4.4 µs per write | 3.7 µs |
| Clock contention | 0.64 / 0.79 / 0.74 µs per write | 0.60 / 0.60 / 0.57 µs |
| Drop churn | 632 µs | 536 µs |
| Rebuilding children | 614 µs | 471 µs |
| Keyed rows | rebuilt 682–739 µs; keyed, all changed, 521 µs | 572–604 µs; 346 µs |
| Wide creation | 67 ms | 42 ms |
| Sweep freeing 10 000 watchers | 128 ms | 107 ms |

- Unchanged within the noise: handles, last-handle drops (8 alternations: −0% and −29% on two),
  discarded values, protocol overhead (+2%: on one thread a spawn is already cheap). Allocations per run
  stay at 10: a fast-path run saves its task but boxes its future.
- Meshes, n=8 over five runs each: 26–38 before, 15–24 after.
- Not measurable on two cores: the padding and the coalesced closes, which cut cross-core traffic on
  8–64 cores. Batched token opens (review item 2.8) were not done: the opens of a framing walk hit a
  fresh epoch no other thread touches yet, and deferring them would let a concurrent disposal close an
  entry's token first and settle the epoch early.
- Loom: 45 models; bound 3 explores ~390 000 interleavings with the fast path and ~654 000 without
  (`LITHIA_LOOM_FAST_PATH=off`), bound 4 ~5.6 million and ~10.2 million; all pass. CI runs both at bound 3.

## Spec v0.4.0, section 15

The benchmark section of spec v0.4.0, kept here when v0.5.0 moved it out of the spec.

First results show that spawning a task per run dominates every cost, so the inline fast path (11.4, step 4) is the largest available win; v0.4.0 adds it (below). The benchmarks live in the `lithia-bench` crate (`cargo bench -p lithia-bench`). The figures below come from a 2-core shared cloud container in release mode, from three runs of v0.3.0 on 2026-11-09 and 2026-11-11, given as a range where the runs differ; they show orders of magnitude and relative costs only. Two runs of v0.2.5, alternated with the last two, fell in the same ranges except where noted.

| Benchmark | Measures | Result | Informs |
| --- | --- | --- | --- |
| Spawn overhead | One task per run, against running a cheap body inline | 4.2–4.5 µs per run spawned, 1–2 ns per body inline | Whether the fast path (11.4, step 4) belongs in v0 |
| Framing | The single-threaded cost of framing a region, including once per write in tasks lithia did not start | fan-out of 1 000: 3.1–4.1 ms per write; 100 writes: 77–189 µs batched in a lithia task, 179–187 µs one batch per write | Whether framing needs caching, and batching advice (6.2) |
| Fan-in contention | Contention on one node's scheduling lock when many parents release it at once | 12 800 writes into a 64-parent join: 7.5–15 µs per write, the join ran 2 300–2 500 times | The lock design (11.4, step 1) |
| Clock contention | Contention on the process-wide version clock under parallel writes | 1.5–1.7 µs per write with 1 writer, 1.6–2.0 µs with 2, 1.6–1.9 µs with 4 | Falling back to one seen version per parent (5, 7.2) |
| Protocol overhead | Single-threaded overhead against a global-lock baseline, set beside MV-RP's 20–25% | 2.0–2.8 µs per node run, about 1 ns per node recomputed under one global lock | Whether lithia's subset of MV-RP is as cheap as expected (11.4) |
| Writer sets in meshes | Runs for n-way bindings, against the n² bound | n = 2: 2 runs; 3: 3; 4: 4; 6: 6–8; 8: 13–15 | Whether accumulation needs tuning (7.4) |
| Drop churn | `spawn_blocking` drops when per-item effects are recreated on every run | 100 per-item effects recreated: 2.1–2.5 ms per rerun | Batching drops (10.2, 10.3) |
| Rerun rates | Typical rerun rates in healthy graphs | one node driven as fast as `flush` allows: 41 000–50 000 runs/s | The default threshold for the rerun-rate warning (12, 17) |

- The protocol-overhead comparison is lopsided by design: the baseline recomputes the graph in a loop with no scheduling at all, so the gap is mostly spawn overhead, not framing or writer sets.
- Batching halves the cost of 100 writes, which supports the advice to write from lithia tasks (6.2).
- Clock contention is mild at this scale: 4 parallel writers cost at most about 25% more per write than one.
- The eight-way mesh took 13–15 runs where v0.2.5, run alternately, took 8. Run counts in meshes depend on scheduling, both stay far below n² = 64, and nothing in v0.3.0 changes how writes propagate, but the difference was consistent and is worth a look (17).
- Meshes stay far below the n² bound, so writer-set accumulation needs no tuning yet.
- A healthy node reruns about 45 times faster than the provisional 1 000/s warning threshold (17).

Lifetime costs, added by v0.3.0, from the same runs:

| Benchmark | Measures | Result | Informs |
| --- | --- | --- | --- |
| Handles | Clone and drop of an outside handle, with its anchoring count, against a bare `Arc`; a read through the handle | 19 ns against 11 ns; a read 20–21 ns | Whether the per-node token design is needed (10.1): the count costs about 8 ns per clone and drop, so it is not |
| Creation | A bound effect, against one held by support, whose temporary handle's drop runs the start check | 0.8–1.0 µs against 0.9–1.7 µs | The cost of support at creation (10.1) |
| Sweep over a hub | Dropping the last handle of a source with 10 000 handle-less watchers: one sweep lets go of all of them, and they are freed | 219–294 ms, against 216–284 ms to free 10 000 scoped watchers by their handles | The sweep's own share is within the noise; freeing dominates, as each child leaves the hub's subscriber list (10.1) |
| Rebuilding children | A rerun that creates 100 children and releases the previous run's 100 | 2.8–3.5 ms per rerun, against 2.1–2.5 ms for value-held per-item effects (drop churn) | Generations cost about as much as children held in a value (10.1) |
| Last-handle drops | A body's `helper(&x.accessor())` idiom on a supported node: a temporary anchor whose drop runs a sweep that ends at its start check | 250–570 ns per drop on one thread; 1.0–3.7 µs on each of two threads at once | The budget of 1 µs per drop on one thread is met; the one sweep lock serializes concurrent drops (16) |

- A last-handle drop can run a sweep on the dropping thread, under one process-wide sweep lock, and every clone and drop of a handle updates an atomic count: that is the price of lifetimes without owners (10.1). The budgets above hold it to a fraction of a run. Two threads dropping temporary anchors at once each pay several times as much as one, through the one lock and the shared atomics; finer-grained locking stays on the roadmap until a real workload shows it (16).
- Freeing many children of one node cost time quadratic in their number, since each left the parent's subscriber list by a scan of it. Since v0.4.0 the list is keyed by id and freeing is linear (the wide-creation row below).

Keyed rows, added by v0.3.1, from ten runs on 2026-11-17 alternated with ten runs of v0.3.0, as the median with the range in brackets:

| Benchmark | Measures | Result | Informs |
| --- | --- | --- | --- |
| Keyed rows | A list node that maps 100 rows to a derived node each, rerun 50 times with one kind of edit, with every row rebuilt as in v0.3.0 against `key = row.id`; the time per list rerun, children included | no row changed: 2.73 ms [2.01–3.40] rebuilt, 78 µs [64–95] keyed; one row changed: 2.76 ms [2.47–3.48] against 90 µs [83–96]; rows reversed: 2.86 ms [2.25–3.06] against 81 µs [76–107]; every row changed: 2.77 ms [2.34–3.27] against 2.34 ms [1.74–2.57] | What reclaiming by key saves over rebuilding (10.1, Keyed nodes) |

- Reclaiming a row whose captures are equal costs under 1 µs, a lookup and a probe of each capture, against about 27 µs to create a row, run it and release the old one. A list rerun that changes few rows is therefore 30 to 35 times cheaper keyed.
- A reordered list costs no more than an unchanged one, since a key follows its row wherever it moves.
- When every row changed, each keyed row is refreshed and reruns in place, which still costs a little less than a new node: about 23 µs against 28 µs per row.
- Nodes created without a key pay nothing measurable: in the same session, every median of v0.3.1's common benchmarks falls within the range of v0.3.0's ten runs. Both ran faster than the figures above, on a different container, so only the comparison between them carries over.

v0.4.0 against v0.3.1, from runs alternated on 2026-11-20 in the same kind of container (two of each, and three of v0.4.0 after the fixes below):

- Every benchmark falls within the range of v0.3.1's runs, keyed rows included (70–90 µs per unchanged list rerun, against 72–90 µs), except two.
- A last-handle drop on a supported node costs 605–641 ns, against 499–528 ns: the temporary anchor's arrival now walks downstream to hold again what support let go of (10.1), which ends at once on a node support holds. It stays within the 1 µs budget.
- Meshes run more, since every cut writer reruns once its epoch settles (7.4): n = 2: 3 runs; 3: 5; 4: 7; 6: 11; 8: 15–22, all still far below n².
- The first v0.4.0 runs found three costs the implementation then removed: a lock-free swap cell in every handle, whose drop scans every thread's slots (a handle's clone and drop took 1.2 µs, now 20 ns, as in v0.3.1); a full delivery for each reclaimed keyed node (keyed rows took 350–440 µs); and an `Arc` clone on every anchoring handle.

A performance pass after a code review, from five runs alternated on 2026-11-20 with five of v0.4.0 as first committed, as medians. Two benchmarks are new: values `equals` discards and replaced strings, and one run creating 10 000 children. Allocations are counted by a counting allocator on a one-thread runtime (`cargo bench -p lithia-bench --bench allocs`).

| Benchmark | Before | After |
| --- | --- | --- |
| Spawn overhead | 4.8 µs per run | 3.9 µs |
| Protocol overhead | 1.8 µs per node run | 1.25 µs |
| Framing | 100 writes: 57 µs batched, 172 µs one batch per write | 35 µs, 123 µs; fan-out unchanged within the noise |
| Fan-in contention | 5.9 µs per write | 3.8 µs |
| Clock contention | 0.90, 1.10 and 1.05 µs per write with 1, 2 and 4 writers | 0.60, 0.64 and 0.59 µs |
| Drop churn | 1.86 ms per rerun | 0.60 ms |
| Rerun rates | 77 000 runs/s | 126 000 runs/s |
| Creation and sweeps | a bound effect 1.0 µs; 10 000 watchers freed by a sweep in 177 ms | 0.86 µs; 147 ms |
| Rebuilding children | 3.3 ms per rerun | 0.65 ms |
| Keyed rows | rebuilt 2.7–2.9 ms per list rerun; keyed, every row changed, 2.2 ms | 0.69–0.75 ms; 0.60 ms; keyed with few changes unchanged at 73–82 µs |
| Discarded values | a run whose value `equals` keeps 18 µs per write and flush; an equal write 9.2 µs; a `String` write 12.5 µs | 8.4 µs; 57 ns; 260 ns |
| Wide creation | one run creating 10 000 children: 315 ms per rerun | 77 ms |
| Allocations | 17 per run on a chain, 14.6 per child run on a fan-out, 24 per derived node created | 10, 7.5, 17 |

- Values `equals` or a superseded run discarded went to the blocking pool whatever their type, against 10.3; they now follow its rule, and strings and byte buffers drop inline too. That is the discarded-values row.
- Gathering a step's drops into one blocking task per runtime (10.3) accounts for the drop-churn, rebuilding and keyed rows: one `spawn_blocking` per freed body or replaced value had been most of their cost.
- A freed child scanned its parent's whole subscriber list to leave it, so freeing many children of one node was quadratic; the list is now keyed by id. With a creator's list of creations pruned only when it doubles, the wide-creation row is linear.
- The other rows gain from fewer allocations and locks per run: an inline mask for the first 64 parents, a small sorted vector for a node's entries instead of a tree whose first insert allocates about 1 KB that the node keeps, no copy of the served epochs and no delivery for a poll that wrote nothing, a waker only for runs that can suspend, a failure record cleared only when there is one, no supersede check on writes to plain source nodes, a `changes()` channel made by the first stream, and a per-thread cache of the scope of a task lithia did not start.
- Handles and last-handle drops are unchanged. Mesh run counts vary with scheduling in both: over fifteen runs each, the eight-way mesh took 15–42 runs before and 18–58 after, medians 21–23 and 25–26, all below n² = 64.
- Spawning a task per run is still the largest cost: 3.9 µs against 1 ns for a cheap body inline. The fast path below takes most of it.

The fast path (11.4, step 4) and padded statics, from five runs alternated on 2026-11-20 with five of the performance pass, as medians:

| Benchmark | Before | After |
| --- | --- | --- |
| Spawn overhead | 3.9 µs per run along a chain | 1.25 µs |
| Framing | a 1 000-wide fan-out: 4.0 ms per write | 1.2 ms; batched and one batch per write unchanged |
| Fan-in contention | 4.4 µs per write | 3.7 µs |
| Clock contention | 0.64, 0.79 and 0.74 µs per write with 1, 2 and 4 writers | 0.60, 0.60 and 0.57 µs |
| Drop churn | 632 µs per rerun | 536 µs |
| Rebuilding children | 614 µs per rerun | 471 µs |
| Keyed rows | rebuilt 682–739 µs per list rerun; keyed, every row changed, 521 µs | 572–604 µs; 346 µs |
| Wide creation | 67 ms per rerun | 42 ms |
| Creation and sweeps | 10 000 watchers freed by a sweep in 128 ms | 107 ms |

- A chain of synchronous nodes now runs in one task, up to 32 runs or 50 µs, so a run costs 1.25 µs instead of 3.9 µs; that is close to the protocol's own cost (protocol overhead, 1.2–1.3 µs per node run on one thread, unchanged).
- A 1 000-wide fan-out on two workers is packed into about 63 runner tasks instead of 1 000 tasks. On more workers it makes about one runner task per worker while each holds at most 16 runs: 1 000 children on 64 workers still make 63 tasks, 100 make 50.
- Allocations stay at about 10 per run: a run on the fast path saves its task but boxes its future.
- Handles, last-handle drops, discarded values and protocol overhead are unchanged within the noise. Mesh run counts still vary with scheduling: the eight-way mesh took 26–38 runs before and 15–24 after in five runs each.
- The statics every step reads or writes (the clock, the node and epoch counters, the settle signal, the drop count and the sweep lock) sat two to four to a cache line; each now has 128 bytes of its own. The two cores of this container cannot show that; it is for the 8 to 64 cores lithia targets.
