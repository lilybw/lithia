<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# A sweep lock that scales

A plan for discussion, on the branch `optimizations/locking`. Nothing here is implemented yet.

## What the measurements say

AMD Ryzen 9 7950X3D (16 cores, 32 threads on two 8-core dies), Windows,
`cargo bench -p lithia-bench --bench scaling`:

| Row | 1 | 2 | 4 | 8 | 16 | 32 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `last-handle`, per drop | 311 ns | 810 ns | 1.94 µs | 6.67 µs | 48.18 µs | 91.04 µs |
| `clock`, per write | 415 ns | 516 ns | 551 ns | 652 ns | 703 ns | 992 ns |

Each thread in `last-handle` drops handles of its own node, so nothing it does needs to wait for
another thread, yet a drop costs 290 times as much at 32 threads as at one. The version clock, one
contended atomic, stays within 2.4 times, so the machine is not the cause.

## What the sweep lock guards

One process-wide mutex (`SWEEP` in `crates/lithia/src/lifetime/support.rs`) is taken by:

| Step | Why it takes the lock |
| --- | --- |
| A rise of an anchor count from zero (`add_anchors`) | A sweep must never see a count rise in its midst, or it could let go of what the new anchor reaches. While support has let go of anything alive (`LET_GO`), the rise also walks downstream and holds it again (`rehold`). |
| Writer registration (`add_writer`) | The same: a supported writer holds again what it may write. |
| Every sweep (`sweep`) | For its whole length: checking each starting point, walking the region downstream of those that lost support, searching for support, letting go. |

`Lifetimes.tla` models this lock: a rise from zero is one step that needs the lock free, and a sweep
holds it from start to end. Five faults found in v0.4.0.2 came from exactly this ordering: a rise
walking only at the end of its operation, a sweep missing a rise during it, and three more
(`docs/tla/README.md`). Any change here is modelled first.

## Where `last-handle`'s time goes

Each `drop(m.accessor())` takes the lock twice:

1. `m.accessor()` raises `m`'s anchor count from zero: lock, add, no walk (nothing is let go).
2. Dropping it lowers the count to zero, which raises `m` as a starting point: lock, a bounded search
   that finds `m` still supported through its anchored parent, unlock. Nothing changes.

Each hold is short, perhaps 100–150 ns including the search's allocations. Fully serialised, 32 threads
would then wait about 10 µs per drop; they wait 91 µs. So most of the cost is the lock changing hands,
waking parked threads and moving its cache line between cores and between the two dies. The jump
between 8 and 16 threads fits contention spreading across dies. A profile should confirm this before
anything changes: a sampling profiler (samply, or Windows Performance Recorder) over the `last-handle`
row, and the `sweeps` row run on the same machine.

## Options

### A. Take no lock when nothing can change

Two targeted changes, each keeping the lock's design.

**A1. A rise from zero without the lock while no sweep runs.** Each sweep raises a `SWEEPING` count
before reading any anchor and lowers it at the end. A rise adds to its anchor count, then reads
`SWEEPING`; only if a sweep is under way, or support has let go of something (`LET_GO`), does it take
the lock as today. With sequentially consistent orderings on both sides, either the sweep reads the
new count or the rise sees the sweep. This is what `Lifetimes.tla`'s `Rise` already assumes (one step,
taken while no sweep holds the lock), so the model changes little.

**A2. No sweep for a starting point that is plainly still supported.** Before raising a node, check
without the lock whether it has an anchored parent or an anchored writer. If that parent's last
anchor falls at the same moment, the fall raises the parent, whose sweep walks downstream through
this node; the check reads the parent after the node's own count has fallen, so that sweep is sure
to see it. Only anchors qualify, not nodes support holds: a sweep already under way may let go of a
supporter after it has passed this node, and nothing would sweep the node again.

Effect: `last-handle`'s drops would take no lock at all, so the row should come out flat. Neither
change helps a workload that really lets go of things: a creator rerunning and releasing its children,
or many hubs dropped at once.

### B. A reader-writer sweep lock with a slot per core

Rises, writer registration, and starting-point checks that find support take the lock shared; a sweep
that lets go of something, and a walk that holds things again, take it exclusively. A sweep whose
check finds a node unsupported gives up its shared hold, takes the lock exclusively and checks again.
Shared holders each touch only their own slot (a "big-reader" lock), so they don't pass a cache line
around; an exclusive holder visits every slot.

Effect: everything that changes nothing scales; sweeps that let go still run one at a time, but they
do real work and are rarer. A and B combine: A removes most shared acquisitions outright.

### C. One sweeper

Starting points go to a queue, and one task sweeps them in batches under the lock. A drop never waits
for a sweep, and one hold covers many starting points. But nodes are freed later than their last drop,
which changes what spec §10.1 promises, and the timing tests, `live_nodes` and the diagnostics rely on.
It also needs a wake-up and back-pressure design, and `settled` would wait for the sweeper.

### D. A lock per part of the graph

Independent parts of the graph would never contend. But parts merge when an edge is added and split
when one is removed, so this needs concurrent union-find, locking two parts at once in a fixed order,
and parts in the lifetime model. It is the largest change and the hardest to verify.

### E. Optimistic sweeps

Sweeps run without the lock and start again if a rise happened meanwhile, detected by a version count.
That count is itself one contended cache line, and a long sweep under constant churn may never finish.

## Proposal

1. Measure first, on the 7950X3D: a profile of `last-handle`, the full scaling table (`sweeps`,
   `rebuild` and `readers` included), and a count of lock acquisitions and hold times per row.
2. A1 and A2. Model each in `Lifetimes.tla` behind constants, as before, so every old model keeps its
   state count; check that undoing the ordering fails a property; then loom models of a rise racing a
   sweep without the lock, and of a parent's fall racing a child's skipped sweep; then kept mutations.
3. Measure again. If `sweeps` or a realistic workload (many tasks rebuilding keyed lists) still
   degrades, do B, modelled the same way.
4. C and D only if B is not enough.

Done means: `last-handle` within twice its one-thread cost at 32 threads; no row of the scaling or spec
benchmarks slower on one thread; every TLA+ model, loom model and test passing; and spec §10.1 and
§11.4 describing the new rule.

## Questions

- Which workloads matter most: many threads dropping handles of independent nodes, or shared hubs and
  rebuilding creators? That decides whether A is enough.
- Is freeing a little later than the last drop acceptable (C), or should a drop keep freeing at once?
- Before 1.0, or in 1.0.x? Nothing here changes the API.
