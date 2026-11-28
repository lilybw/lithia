# Benchmarks

`cargo bench -p lithia-bench` prints one table of results; `LITHIA_BENCH=spawn,clock` runs only the named
rows, and `cargo bench -p lithia-bench --bench allocs` counts heap allocations. Every figure here comes
from a 2-core shared cloud container in release mode, unless it names another machine. They show orders of
magnitude and relative costs: measure on your own machine before relying on them.

## Scaling with threads

The spec rows run on one to four threads. `cargo bench -p lithia-bench --bench scaling` runs only when
asked for, and measures lithia's shared state as threads are added: one column per thread count, the
powers of two up to the machine's hardware threads and that count itself, or the counts in
`LITHIA_BENCH_THREADS=1,8,64`. `LITHIA_BENCH` picks rows, as for the spec rows. `LITHIA_BENCH_PIN=dense`
pins worker `i` to logical CPU `i`, and `spread` to every other logical CPU first, which on most machines
is one per physical core first; a build with `RUSTFLAGS="--cfg lithia_lock_stats"` follows each cell with
what the sweep lock went through: acquisitions, mean and longest hold, mean wait; and `--features mimalloc`
runs it on mimalloc instead of the system allocator, which tells the allocator's share of a cost. Rows that time one
operation per thread give its median and, in brackets, the fastest and slowest thread: a wide range is
threads waiting for each other.

| Row | Shared state | A cell is | Reads well when |
| --- | --- | --- | --- |
| `last-handle` | the sweep lock | per last-handle drop, one dropping body per thread | flat: no contention |
| `sweeps` | the sweep lock, freeing | per creation / per hub drop (sweep and freeing), each thread creating 2 000 effects support holds, then all dropping their hubs at once | flat |
| `sweep-10`, `sweep-100` | the sweep lock, freeing | per hub drop, with 10 or 100 effects per hub | flat |
| `sweep-kept` | the sweep lock | per hub drop whose 2 000 effects also read an anchored source: the sweep walks them and lets go of none, so nothing is freed | flat |
| `sweep-turns` | freeing, the allocator | per hub drop of 2 000 effects, the hubs dropped one at a time, so sweeps never overlap | flat: one sweep alone |
| `churn` | all of them | per cycle of a hub and 10 effects created, flushed and dropped, 200 per thread | flat |
| `create-source` | ids, the allocator | per creation / per free of plain source nodes, which link to nothing | flat |
| `create-shared` | a hub's subscriber list | per effect created on one hub every thread shares | rising: one hub is one lock |
| `alloc` | the allocator, no lithia | per allocation / per free of a node-sized box | flat |
| `counter` | one atomic, no lithia | per addition to one shared counter, as each creation adds to `NodeId`'s | rising: it is one cache line |
| `clock` | the version clock | per write, each thread writing a node of its own | flat |
| `readers` | value cells | per write rerunning a 10-node chain on 2 workers, while that many threads read it | flat |
| `fan-out` | runner tasks | per write to 1 000 children, on that many workers | falling: workers help |
| `fan-in` | scheduling locks | per write, 64 writer tasks into one join, on that many workers | flat or falling |
| `rebuild` | generations, drops | per rerun rebuilding 100 children, on that many workers | flat |

A sweep-lock row that rises with threads while `clock` stays flat points at the sweep lock rather than at
the machine; with the lock's figures, a hold that stays flat while the wait grows is the lock's queue, a
hold that grows is work under it, and a cell that grows while both stay small is elsewhere: `alloc` and
`counter` show what the allocator and one shared counter cost alone. Record a run here with the machine's model and core count, as in the history below.

### Comparing sweep locks

`python3 .github/scripts/lock_bench.py` runs the rows the sweep lock takes part in (or those after
`--rows`) once per lock, with the lock's figures under `--stats`, each built with `--cfg
lithia_sweep_lock="NAME"` in its own directory under `target/locks`, and prints a table per row with a
lock on each line; `--locks` and `--threads` narrow it. The locks: `parking_lot`'s mutex as it is (the
default), the same handing every unlock to the next waiter (`fair`), the standard library's (`std`),
`parking_lot`'s tried for about ten microseconds before parking (`spin`), and a ticket lock that spins,
then yields, and never parks (`ticket`). `check.py full` lints lithia built with each.

`python3 .github/scripts/bench_plot.py RESULTS.txt --out plot.html` charts the output of either, saved
to a file: a chart per row and measure against the thread count, the median thread as a line and the
fastest to the slowest as a band, and the lock's hold and wait under `--stats`.

## Current figures

Measured on v0.5.0 after the code review's tuning (2026-11-24), as medians of 14 runs alternated with a
build from before the review; the last column gives the change. Run-to-run noise on this container is
about ±5%.

| Benchmark | What it measures | Result | Change |
| --- | --- | --- | --- |
| Run along a chain | A synchronous node run on the fast path | 1.0 µs per run | −14% |
| Protocol overhead | A node run in a 100-node diamond mesh | 1.0 µs per run | −21% |
| Framing | A write into a 1 000-wide fan-out | 1.0 ms per write | −9% |
| Batching | 100 writes, batched in a lithia task against one batch per write | 28 µs against 103 µs | −26%; −9% |
| Fan-in contention | 12 800 writes into a 64-parent join | 3.3 µs per write | −11% |
| Clock contention | Writes from 1, 2 and 4 parallel writers | 0.50, 0.31 and 0.33 µs per write | −14%; −44%; −42% |
| Writer sets in meshes | Runs for an 8-way binding mesh, against the bound n² = 64 | 15–24 runs, depending on scheduling | |
| Drop churn | 100 per-item effects recreated on each rerun | 381 µs per rerun | −24% |
| Rebuilding children | A rerun that creates 100 children and releases the last run's 100 | 310 µs per rerun | −22% |
| Keyed rows | 100 rows per list rerun: rebuilt; keyed, every row changed; keyed, few changed | 367–409 µs; 409 µs; 68–74 µs | −20 to −31%; −15%; within noise |
| Wide creation | 10 000 children created per run | 29 ms per rerun | −25% |
| Sweep over a hub | Letting go of and freeing 10 000 handleless watchers | 111 ms | −9% |
| Rerun rates | One node driven as fast as `flush` allows | about 143 000 runs/s | |
| Discarded values | A run whose value `equals` keeps; an equal write; a `String` write | 6.8 µs; 55 ns; 124 ns | −8%; −4%; −55% |
| Handles | Clone and drop of a handle, against 11 ns for a bare `Arc`; a read | about 20 ns; 19 ns | within noise; −17% |
| Creation | A bound effect; one held by support | 0.85 µs; 1.1 µs | within noise |
| Last-handle drops | The `helper(&x.accessor())` idiom on a supported node, one thread; two | 533 ns; 2.7 µs | −4%; −10% |
| Allocations | Per run on a chain; per child run on a fan-out; per outside write; per node created | 3.2; 2.9; 11; 11.6 | from 10.2; 7.9; 17; 16.6 |
| Reader threads | A chain run after 0, 16 and 64 threads have each read every node once | 1.0 µs at each | was 1.4, 1.7 and 2.1 µs |

- 100 writes batched in one lithia task cost under a third of what 100 separate batches cost.
- Clock contention stays mild at this scale: four parallel writers cost no more per write than one.
- Lifetimes are cheap where it matters. The anchoring count adds about 8 ns to a handle's clone and
  drop, and a last-handle drop stays within its 1 µs budget. Concurrent drops contend on the one sweep
  lock, which two cores cannot measure.
- Keys pay off when few rows change: a reclaim with equal captures costs under 1 µs, against about
  27 µs to rebuild a row. When every row changes, refreshing in place is still a little cheaper.
- Mesh run counts vary with scheduling and stay far below n².

## History

### mimalloc against the system allocator (2026-11-28)

The same 7950X3D and build, `lock_bench.py` without `--stats`, once with the system allocator and once
with `--features mimalloc`. Each cell is the range over the five locks; `ticket`'s outliers aside.

| Row | 1 thread: system → mimalloc | 32 threads: system → mimalloc |
| --- | ---: | ---: |
| `sweeps`, per creation | 654–717 → 537–572 ns | 6.4–6.8 → 2.3–2.9 µs |
| `sweeps`, per drop | 2.2–2.4 → 1.4–1.5 ms | 31–32 → 18–40 ms |
| `sweep-kept`, per drop | 0.90–1.00 → 0.61–0.79 ms | 26–28 → 17–22 ms |
| `churn`, per cycle | 26–36 → 25–33 µs | 0.33–0.49 → 0.22–0.55 ms |
| `create-shared`, per creation | 518–612 → 332–509 ns | 25–26 → 20–22 µs |
| `rebuild`, per rerun | 141–148 → 113–119 µs | 139–162 → 108–126 µs |

- Most of what creation lost on many threads was Windows' heap: at 32 threads a creation costs 2.6 µs
  with mimalloc instead of 6.6. Against 0.55 µs on one thread, the rest is lithia's own shared state.
- A sweep that keeps everything is a quarter faster: it allocates its maps under the sweep lock.
- Drops at 32 threads and creation on a shared hub hardly move: they wait for a lock.

### Drop threads on the 7950X3D (2026-11-28)

`lock_bench.py --stats` on 753a3f3, system allocator. Each after is the range over the five locks, with
`ticket` in brackets where it is slower than all of them; the lock's figures are `parking_lot`'s.
Before is `parking_lot` in the entry below.

| Row | 1 thread, before → after | 32 threads, before → after | Lock at 32: held, waited |
| --- | ---: | ---: | ---: |
| `sweeps`, per drop | 13.6 → 2.1–2.5 ms | 235 → 31–42 ms (ticket 69) | 2.0 ms, 26 ms |
| `sweep-100`, per drop | 2.22 ms → 99–137 µs (381) | 17.7 → 1.3–2.9 ms (4.3) | 0.11 ms, 1.5 ms |
| `sweep-10`, per drop | 245 → 17–73 µs | 2.36 → 0.15–0.29 ms (0.43) | 13 µs, 175 µs |
| `sweep-turns`, per drop | 17.3 → 2.1–3.9 ms (4.6) | 40 → 3.7–5.9 ms | 1.5 ms, 0 |
| `churn`, per cycle | 59 → 22–33 µs (50) | 1.17 → 0.30–0.48 ms (0.54) | 10 µs, 337 µs |
| `sweep-kept`, per drop | 0.93 ms, unchanged | 27.5 ms, unchanged | 2.0 ms, 26 ms |
| `create-shared`, per creation | 590 ns, unchanged | 25 µs, unchanged | |

- Sweeps that free are 3 to 20 times as fast on one thread, and 6 to 15 times at 32.
- Now the sweep lock is most of what remains on many threads: at 32, a 2 000-effect drop waits 26 of its
  31–42 ms for it, a churn cycle 337 of 483 µs. A hold of the same work grows from 0.7 ms on one
  thread to 2 ms at 32.
- Creation is unchanged, 640 ns on one thread and 6.9 µs at 32, and so is creation on a shared hub.
- The second run's `--features mimalloc` was ignored: `lock_bench.py` did not pass it on, and now
  refuses arguments it does not know.

### The sweep lock's own figures, and drop threads (2026-11-28)

The same 7950X3D, system allocator, `lock_bench.py --stats`; `parking_lot` shown, the other four within
the run-to-run spread of it, as before. Each cell is the median thread's time, then the lock's mean hold
and mean wait per acquisition:

| Row | 1 | 8 | 32 |
| --- | ---: | ---: | ---: |
| `sweeps`, per drop | 13.6 ms; 0.77 ms, 0 | 70.1 ms; 1.35 ms, 3.5 ms | 235 ms; 1.74 ms, 22.6 ms |
| `sweep-kept`, per drop | 0.95 ms; 0.76 ms, 0 | 4.7 ms; 1.12 ms, 3.0 ms | 28.7 ms; 1.61 ms, 23.6 ms |
| `sweep-100`, per drop | 2.22 ms; 40 µs, 0 | 7.1 ms; 43 µs, 118 µs | 17.7 ms; 68 µs, 0.99 ms |
| `sweep-10`, per drop | 245 µs; 10 µs, 0 | 781 µs; 9 µs, 33 µs | 2.36 ms; 11 µs, 164 µs |
| `churn`, per cycle | 59 µs; 4.5 µs, 0 | 393 µs; 8.5 µs, 3.0 µs | 1.17 ms; 12.2 µs, 52 µs |
| `create-shared`, per creation | 654 ns | 3.6 µs | 25.5 µs |

With mimalloc, `last-handle` stays at 21–26 ns and `clock` rises from 306 ns to 834 ns per write.

- Where a sweep frees, the lock is a tenth of a drop's time at 32 threads and a twentieth on one: a
  hub of 10 effects took 245 µs to drop while holding the lock for 10 µs. The rest was tokio's blocking
  pool, whose threads woke and contended for each parcel of 64 bodies a sweep handed on: one 2 000-effect
  drop made 45 000 futex calls on this container. Deferred drops now go to lithia's own drop threads
  (spec §10.3). On 2 cores, medians of three alternated runs, one thread and two:

  | Row | Blocking pool | Drop threads |
  | --- | ---: | ---: |
  | `sweeps`, per drop | 22.4 ms; 21.7 ms | 5.4 ms; 5.5 ms |
  | `sweep-100`, per drop | 1.59 ms; 1.49 ms | 106 µs; 88 µs |
  | `sweep-10`, per drop | 123 µs; 207 µs | 36 µs; 14 µs |
  | `sweep-turns`, per drop | 22.0 ms; 22.3 ms | 5.6 ms; 5.7 ms |
  | `churn`, per cycle | 104 µs; 213 µs | 37 µs; 44 µs |

  `sweep-kept`, `create-source` and `rebuild` free nothing through the pool and do not move.
- Where a sweep keeps everything, the lock is the time: at 32 threads 24 of 29 ms are spent waiting,
  and the hold itself doubles, the same work done with 31 other sweeps in flight.
- Creation on one shared hub grows 40 times by 32 threads, without the sweep lock: each new child
  takes the hub's scheduling lock to subscribe.

### Sweep lock taken only where something changes, and five locks compared (2026-11-28)

The same 7950X3D, at f91a039 on `optimizations/locking`: rises from zero and falls on a supported node
take no lock (spec §10.1). `python3 .github/scripts/lock_bench.py`, one run per lock:

| Row | Lock | 1 | 2 | 4 | 8 | 16 | 32 |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `last-handle`, per drop | `parking_lot` | 20 ns | 21 ns | 21 ns | 21 ns | 22 ns | 26 ns |
| | `fair` | 20 ns | 21 ns | 20 ns | 21 ns | 23 ns | 26 ns |
| | `std` | 20 ns | 20 ns | 20 ns | 21 ns | 24 ns | 24 ns |
| | `spin` | 22 ns | 20 ns | 20 ns | 21 ns | 22 ns | 26 ns |
| | `ticket` | 20 ns | 21 ns | 20 ns | 20 ns | 24 ns | 24 ns |
| `sweeps`, per creation / per sweep | `parking_lot` | 645 ns / 14.53 ms | 626 ns / 6.17 ms | 725 ns / 35.08 ms | 897 ns / 67.73 ms | 3.11 µs / 151.96 ms | 6.58 µs / 293.81 ms |
| | `fair` | 646 ns / 20.63 ms | 665 ns / 4.45 ms | 721 ns / 22.97 ms | 1.45 µs / 148.78 ms | 2.78 µs / 138.57 ms | 6.84 µs / 242.84 ms |
| | `std` | 678 ns / 10.22 ms | 618 ns / 14.49 ms | 750 ns / 44.88 ms | 1.03 µs / 79.74 ms | 3.01 µs / 222.91 ms | 6.54 µs / 383.80 ms |
| | `spin` | 684 ns / 14.62 ms | 610 ns / 16.09 ms | 771 ns / 37.30 ms | 977 ns / 129.50 ms | 2.89 µs / 173.00 ms | 6.45 µs / 583.35 ms |
| | `ticket` | 702 ns / 16.28 ms | 652 ns / 17.07 ms | 786 ns / 49.25 ms | 1.04 µs / 74.83 ms | 2.90 µs / 134.01 ms | 6.38 µs / 362.01 ms |
| `rebuild`, per rerun | `parking_lot` | 151.49 µs | 141.63 µs | 129.49 µs | 151.28 µs | 146.14 µs | 145.21 µs |
| | `fair` | 154.99 µs | 145.50 µs | 130.57 µs | 145.39 µs | 146.94 µs | 137.16 µs |
| | `std` | 149.75 µs | 138.83 µs | 146.58 µs | 139.30 µs | 141.15 µs | 145.05 µs |
| | `spin` | 151.14 µs | 138.69 µs | 134.92 µs | 153.63 µs | 139.28 µs | 137.49 µs |
| | `ticket` | 151.06 µs | 144.53 µs | 136.30 µs | 139.16 µs | 139.30 µs | 148.29 µs |

- A last-handle drop on a supported node no longer takes the lock: 20 ns on one thread instead of 311 ns,
  and 26 ns on each of 32 instead of 91 µs.
- The lock makes no difference beyond run-to-run noise in any row, so `parking_lot` stays.
- What still grows is real work. 32 sweeps of 2 000 nodes each wait about half of 32 sweeps' work in
  turn, as one lock predicts, whichever lock it is. A creation costs ten times as much at 32 threads,
  though it takes the sweep lock nowhere: the allocator (Windows' heap) and the shared counters a
  creation touches (the `NodeId` counter, a per-site count) are the suspects, for a profile to confirm.
  A sweep's figure is the whole `drop(hub)`: the sweep and freeing the 2 000 effects.

### Thread scaling on a 16-core desktop (2026-11-28)

AMD Ryzen 9 7950X3D (16 cores, 32 threads, two 8-core dies, one with 3D V-Cache), 64 GB, Windows, at
0b2da25, `cargo bench -p lithia-bench --bench scaling` with `LITHIA_BENCH=last-handle,clock`:

| Row | 1 | 2 | 4 | 8 | 16 | 32 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `last-handle`, per drop | 311 ns | 810 ns | 1.94 µs | 6.67 µs | 48.18 µs | 91.04 µs |
| `clock`, per write | 415 ns | 516 ns | 551 ns | 652 ns | 703 ns | 992 ns |

The sweep lock does not scale: a last-handle drop costs 290 times as much at 32 threads as at one, though
each thread drops its own nodes. Fully serialised, 32 threads at 311 ns each would cost about 10 µs, so
the lock's waiters cost far more than its work, and the jump between 8 and 16 threads fits contention
spreading across the two dies. The version clock stays within 2.4 times, so the machine is not the
cause. Work on a replacement is on the `optimizations/locking` branch.

### v0.5.0 code review tuning (2026-11-24)

Measured against a build from before the review, alternated over 14 runs. Each change was profiled with
callgrind on the protocol benchmark, whose instruction count fell from 232 to 180 million (−22%); the
review's safety nets had first added 6%, mostly the checked token lists, and the tuning took that back:

- A step skips the parts it left empty (−8% of instructions by itself).
- The fast path reads the clock every fourth run, and a node's rerun counter every 1 024th, under the
  lock its run start already holds.
- Subscribers, children, token and served lists stay inline while small, and a step reuses its thread's
  release buffer; an inline run is boxed only if its first poll leaves it pending. Allocations per chain
  run fell from 10.2 to 3.2.
- The value cell is a reader-writer lock instead of `arc-swap`. An `arc-swap` write walks a list kept by
  every thread that has ever read the cell, so a chain run cost 1.4 µs with no reader threads, 1.7 µs
  after 16 had read and 2.1 µs after 64; the lock keeps it at 1.0 µs. On two cores it also made writes
  10–14%, drop churn 28% and rebuilding children 37% cheaper.
- Smaller items: the change-stream lock only once a stream exists, a run's scope set once without a lock,
  release stores for single-writer run fields, async bodies and leased blocks boxed once, runners spawned
  unboxed, and a per-site atomic for the support pile-up warning instead of a process-wide map.


Each section compares one version with the one before, alternated in the same container. Containers
differed between sessions, so compare figures within a section only.

### v0.3.0 baseline (2026-11-09 and 2026-11-11)

Three runs of v0.3.0, given as a range where they differ. Two runs of v0.2.5 alternated with the last
two fell in the same ranges except where noted. v0.3.0 ran every run as a task of its own.

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

v0.3.1 against v0.3.0 (2026-11-17, ten alternated runs each): every median of v0.3.1's common benchmarks
falls within v0.3.0's range, so unkeyed nodes pay nothing for keys.

### v0.4.0 against v0.3.1 (2026-11-20)

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

### v0.4.0 performance pass (2026-11-20)

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

### v0.4.0 fast path (2026-11-20)

`crates/lithia/src/exec/fast_path.rs`, spec §11.4 step 4. A run or runner task keeps the ready synchronous runs
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
