<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# A sweep lock that scales

A plan for discussion, on the branch `optimizations/locking`.

Status (2026-11-28): approach 1 is implemented, modelled and checked (spec §10.1, `docs/tla/README.md`),
and approach 2a has its benchmark, `.github/scripts/lock_bench.py`. Next: the scaling table and the
lock comparison on the 7950X3D, then 2b only if sweeps that let go still degrade.

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

## How others have solved it

Support is reachability from anchors: the question a tracing collector asks of its roots. A sweep
answers it locally, from the nodes whose count fell, much as trial deletion does for cycles in
reference-counted systems, where a decrement that leaves a count above zero buffers the object as a
possible root [1]. Every concurrent collector must keep a root from appearing unseen in the middle of
a trace, and the answers fall into three families.

**Synchronise only while a collection runs.** Go's collector turns its write barrier on with a fence
at the start of marking [2]. Linux's per-CPU reader-writer semaphore lets a reader bump a per-CPU count
and go on unless a writer has blocked readers; a writer blocks them, then waits until the per-CPU
counts sum to zero [3]. BRAVO adds the same bias to any reader-writer lock: readers record themselves
in a shared table at a slot hashed from thread and lock, so they rarely touch the same line [4]. Linux's
`percpu_ref` counts per CPU and cannot see zero until it is switched to one atomic count [5]; a
scalable non-zero indicator answers "is anyone here?" without a shared counter, because only changes
between zero and non-zero travel up its tree [6].

**Make each synchronisation cheap.** Queue locks spin on a line of their own; NUMA-aware ones keep
handoffs on one socket, which CNA reports as 39 % faster than a plain queue lock on two sockets and 97 %
on four, in a key-value benchmark [7, 8]. Flat combining lets the lock holder apply the requests other
threads post, while they spin on their own records; for a FIFO queue it reports 4–7 times the throughput
of the lock-free Michael–Scott queue [9]. Delegation goes further with a dedicated server thread, up to
an order of magnitude over locks, at the cost of a hardware thread [10].

**Move the work elsewhere.** Bacon and Rajan's concurrent cycle collector never halts all mutators: it
checks its candidate cycles again in the next epoch, freeing only those no increment touched and no
outside reference reaches [1]. samsara sends each drop to a collector thread over a channel, and a
mutator may block while the collector drains it [11]. Free-threaded CPython stops the world for its
cycle collector [12]. MobX, single-threaded, queues a node that lost its last observer and suspends it
when the outermost batch ends [13], as lithia's operation context does with starting points.

Frameworks with an owner tree, such as Solid and Leptos, dispose by ownership and never trace. lithia
has no owner hierarchy by design (spec §10), so it must.

The measurements point at the cost of each acquisition, not at the work done under the lock: 91 µs
per drop where fully serialised holds would cost about 10 µs. So the levers that matter are fewer
acquisitions (the first family) and cheaper ones (the second). The third matters only if sweeping
itself saturates a core.

## Approaches

### 1. Skip the lock when nothing can change

**1a. A rise from zero without the lock while no sweep runs.** Each sweep raises a `SWEEPING` count
before reading any anchor and lowers it at the end. A rise adds to its anchor count, then reads
`SWEEPING`; only if a sweep is under way, or support has let go of something that still lives
(`LET_GO`), does it take the lock as today. With sequentially consistent orderings on both sides,
either the sweep reads the new count or the rise sees the sweep and waits for it, which orders the
rise after the sweep, exactly as a lock would. This is the per-CPU semaphore's fast path, with the
node's own anchor count as the reader's slot: sweeps read those counts anyway, so a rise registers
nowhere else.

**1b. No sweep for a starting point that is plainly still supported.** Before raising a node, check
without the lock whether it has an anchored parent or an anchored writer. If that parent's last
anchor falls at the same moment, the fall raises the parent, whose sweep walks downstream through
this node; the check reads the parent after the node's own count has fallen, so that sweep is sure to
see it. Only anchors qualify, not nodes support holds: a sweep already under way may let go of a
supporter after it has passed this node, and nothing would sweep the node again.

Fixes `last-handle` and every temporary handle on a supported node: a helper's `&x.accessor()`, a
clone handed to a task. Costs: on x86 the rise's add is already a full fence, so the extra cost is one
load of a line that changes only when a sweep starts or ends; under many sweeps that line moves
between cores, and a non-zero indicator [6] would spread it if it ever shows in a profile. No change
to the API or to when anything is freed. Needs a TLA+ model, loom models and mutations, since the
ordering is the whole point.

### 2. Make each remaining acquisition cheap

**2a. A lock that suits short holds.** Holds last about 100–150 ns. `parking_lot`'s mutex unlocks
unfairly by default [14] and parks waiters, so a handoff may cost a wake-up of microseconds while
another thread barges in. Candidates: `std::sync::Mutex` (a futex on Linux, an SRW lock on Windows), a
queue lock that spins on its own line for a few microseconds before parking, and one that prefers
waiters on the holder's die. Fixes every row that sweeps, by a constant factor only a measurement can
tell. Nothing in the model changes, since a lock stays a lock. A
hand-written lock is unsafe code with a loom model of its own, and finding dies differs by platform.
Cheapest to try: a `cfg` that picks the lock, and the scaling benchmark.

**2b. Combining.** A sweep that finds the lock held posts its starting points in a slot of its own and
waits on that slot. The holder, before unlocking, sweeps whatever was posted, for a bounded number of
rounds, then marks those slots done. A drop still returns only after its starting points have been
swept, so spec §10.1's timing holds. Fixes many sweeps that really let go at once (`sweeps`, many
creators rebuilding, a large teardown): one handoff per batch, and regions that overlap walked once.
Costs: the holder walks other threads' nodes, which sit in their caches, so each one is a remote miss,
slower still across dies; the holder's own drop takes longer, bounded by the round limit, which on a
tokio worker stalls that worker; a panic in a value's `Drop` during another thread's sweep must reach
that thread. The lifetime model gains sweeping on another's behalf, a moderate change.

### 3. A reader-writer lock with a shard per thread

`crossbeam_utils::sync::ShardedLock` already does this: a read locks one shard chosen by thread, a
write locks every shard in turn [15]. Start checks that find support would take it shared. After 1,
few such holds remain, and what does remain, sweeps that let go, takes it exclusively and pays for
every shard, so it can slow down the case still contended. A check that finds a node unsupported must
also give up its shared hold, take the lock exclusively and check again. Worth it only if a profile
after 1 shows many holds that change nothing.

### 4. Sweep in the background

A drop pushes its starting points to a queue and returns; a sweeper task sweeps in batches. Every
drop gets cheap. But nodes are freed later than their last drop, which changes spec §10.1, what
`live_nodes`, `settled` and the first-run warning see, and needs a wake-up and back-pressure design
[11]; a sweeper task also meets runtime shutdown. 2b batches as well and keeps the timing.

### 5. Sweep in parallel

Only this lets sweeps of independent parts of the graph run at once. A lock per part needs concurrent
union-find as edges come and go. Sweeps without the lock need a check before letting go, as Bacon
and Rajan's tests do [1]. Either way, today's border rule (a node held by support counts as supported)
relies on sweeps never overlapping: two sweeps at once could each count on a node the other is about to
let go. It is the largest change to code and model and the hardest to verify, and is needed only if
sweeping itself saturates a core. At about the cost of framing a write per node walked, that means
millions of nodes let go per second.

## Compared

| Approach | `last-handle` | Many sweeps that let go | Freed at the drop | Model change | Risk |
| --- | --- | --- | --- | --- | --- |
| 1. Skip | flat | unchanged | yes | small | ordering |
| 2a. Better lock | constant factor | constant factor | yes | none | unsafe lock code |
| 2b. Combining | fewer handoffs | one handoff per batch | yes | moderate | holder latency, panics |
| 3. Sharded lock | helps checks | slower | yes | small | exclusive cost |
| 4. Background | flat | cheap drops | no | large | timing, back-pressure |
| 5. Parallel | flat | scales | yes | largest | correctness |

## A balanced plan

1 and 2 together keep every promise lithia makes and remove the two costs the measurements show:
acquisitions that change nothing, and the price of each handoff. The cost that remains is real
sweeping, done on one core at a time.

1. Measure on the 7950X3D: a profile of `last-handle`, the full scaling table, and the number and
   length of lock holds per row.
2. Approach 1 (1a, 1b). Model each in `Lifetimes.tla` behind constants, so every old model keeps its
   state count; check that undoing the ordering fails a property; then loom models of a rise racing
   a sweep without the lock, and of a parent's fall racing a child's skipped sweep; then kept
   mutations.
3. Approach 2a: `python3 .github/scripts/lock_bench.py` compares five locks, each chosen by
   `--cfg lithia_sweep_lock` (docs/BENCHMARKS.md, Comparing sweep locks); keep the best.
4. If `sweeps` or many creators rebuilding still degrade, approach 2b, modelled first.
5. Approach 3 only if a profile asks for it; 4 and 5 are not planned.

Done means: `last-handle` within twice its one-thread cost at 32 threads; `sweeps` letting go at least as
many nodes per second across 32 threads as on one; no row of the scaling or spec benchmarks
slower on one thread; every TLA+ model, loom model and test passing; and spec §10.1 and §11.4
describing the new rule.

## Questions

- Which workloads matter most: many threads dropping handles of independent nodes, or shared hubs and
  rebuilding creators? That decides whether 2b is needed.
- Under 2b a drop may wait while another thread sweeps for it. Is that acceptable, as long as it still
  returns only after its own nodes are swept? If freeing a little later than the drop were acceptable
  too, the drop could post and return without waiting, which is simpler.
- Before 1.0, or in 1.0.x? Nothing here changes the API.

## Sources

1. D. F. Bacon, V. T. Rajan, [Concurrent Cycle Collection in Reference Counted
   Systems](https://pages.cs.wisc.edu/%7Ecymen/misc/interests/Bacon01Concurrent.pdf), ECOOP 2001.
2. A. Clements, R. Hudson, [Eliminate STW stack
   re-scanning](https://go.googlesource.com/proposal/+/refs/heads/master/design/17503-eliminate-rescan.md),
   Go design proposal 17503.
3. Linux, [kernel/locking/percpu-rwsem.c](https://android-kvm.googlesource.com/linux/+/refs/tags/v5.3/kernel/locking/percpu-rwsem.c).
4. D. Dice, A. Kogan, [BRAVO: Biased Locking for Reader-Writer Locks](https://arxiv.org/abs/1810.01553),
   USENIX ATC 2019.
5. Linux, [include/linux/percpu-refcount.h](https://android.googlesource.com/kernel/common/+/21363d9cdc9597338884ccab173796eb07d9a8ff/include/linux/percpu-refcount.h).
6. F. Ellen, Y. Lev, V. Luchangco, M. Moir, [SNZI: Scalable NonZero
   Indicators](https://research.cs.wisc.edu/trans-memory/biblio/bibhtml/ellen_nonzero-indicators_podc_2007.html),
   PODC 2007.
7. D. Dice, A. Kogan, [Compact NUMA-Aware Locks](https://arxiv.org/pdf/1810.05600), EuroSys 2019.
8. D. Dice, V. J. Marathe, N. Shavit, [Lock Cohorting: A General Technique for Designing NUMA
   Locks](https://researchr.org/publication/DiceMS12), PPoPP 2012.
9. D. Hendler, I. Incze, N. Shavit, M. Tzafrir, [Flat Combining and the Synchronization-Parallelism
   Tradeoff](https://people.csail.mit.edu/shanir/publications/Flat%20Combining%20SPAA%2010.pdf), SPAA 2010.
10. S. Roghanchi, J. Eriksson, N. Basu, ffwd: delegation is (much) faster than you think, SOSP 2017;
    [summary](https://blog.acolyer.org/2017/12/04/ffwd-delegation-is-much-faster-than-you-think/).
11. [samsara](https://github.com/chc4/samsara), a concurrent cycle collector for Rust.
12. CPython, [garbage collector design](https://chromium.googlesource.com/external/github.com/python/cpython/+/fb8d8d9c9f9cbc94fa58887e13e63bf33fe0caac/InternalDocs/garbage_collector.md),
    free-threaded build.
13. MobX, [src/core/observable.ts](https://app.unpkg.com/mobx@6.16.1/files/src/core/observable.ts):
    `queueForUnobservation` and `endBatch`.
14. [`parking_lot::Mutex`](https://docs.rs/parking_lot/latest/parking_lot/type.Mutex.html), on fairness.
15. [`crossbeam_utils::sync::ShardedLock`](https://docs.rs/crossbeam-utils/0.8.19/crossbeam_utils/sync/struct.ShardedLock.html).
