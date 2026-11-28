<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# Creating and freeing nodes on many threads

A survey for discussion, on `optimizations/locking`, after the sweep lock stopped being taken where
nothing changes (`locking.md`). Nothing here is implemented except where it says so.

## What the measurements say

On the 7950X3D, 32 threads each dropping a hub of 2 000 effects wait about 294 ms per drop, against
14.5 ms on one thread, and a creation costs 6.6 µs on each of 32 threads against 645 ns on one, though
it takes no lithia lock (`BENCHMARKS.md`). The figures from this 2-core container, with the new rows
and the lock's own figures, already split that time:

- A drop of a 2 000-effect hub takes about 22 ms here, but holds the sweep lock for only 0.84 ms. The
  rest is freeing: about 5 500 instructions per effect (callgrind), against about 3 200 for sweeping it.
- Freeing a plain source node cost 10 µs. Every node that compares by default (D2) held its comparator
  as an `Arc`'d closure, and each freed node handed it to tokio's blocking pool as a task of its own,
  for which tokio started threads. Fixed (a259489): the default comparator is a plain function, and a
  free now costs about 250 ns.
- Effects still hand their bodies to the pool, once per operation rather than once per node.

So the next runs on the 7950X3D decide between three causes: the sweep lock's serial work (its hold
grows), the allocator (`--features mimalloc` moves it), or lithia's own per-node work (neither does).

## Approaches

### Do less per node

1. **Free in place what runs no user code.** Done for comparators. A body that captures only lithia
   handles and plain values runs no user code when dropped either; dropping it in place, outside the
   protocol's locks, would spare the deferred drop for most effects. The rule that drops never run
   inside the protocol (spec §10.3) stays: only the place changes.
2. **Sweep with marks, not maps.** A sweep keeps its region, its answers and its search in ordered maps
   and vectors, which allocate per node. A sweep number stamped in each node's header, as a tracing
   collector's mark bits are, needs no allocation, and a region that is a tree needs no search at all:
   every node in it lost its only supporter. Bacon and Rajan cut the roots their cycle collector
   considers by an order of magnitude by skipping objects that cannot be part of a cycle [1].
3. **Fewer allocations per node.** A node is several allocations (its record, its subscriber map, its
   parent list, its body). Each one is a trip to the allocator on creation and another on freeing.

### Free differently

4. **A drop queue that never spawns.** Payloads that must drop off the protocol go to tokio's blocking
   pool, which starts a thread whenever none is idle [2]. A few dedicated drop threads fed by per-thread
   buffers, handed over in batches with one atomic exchange, as snmalloc returns freed memory to the
   thread that owns it [3], cost one hand-off per batch and no thread start. Epoch-based reclamation
   amortises frees the same way, from per-thread garbage lists [4].
5. **An allocator that scales.** lithia cannot choose the program's allocator, but it can measure and
   recommend one. Windows' CRT heap "doesn't scale well on large core count machines": linking clang
   with ThinLTO on 36 cores took 38 min 47 s on it and 2 min 22 s on mimalloc [5]; mimalloc shards its
   free lists per page and handles frees from other threads apart from local ones [6]. Short of that,
   node records could come from a pool of lithia's own, per thread, such as a sharded slab, which beats a
   slab behind a reader-writer lock once several threads use it [7].
6. **A region per generation.** The nodes one run creates are released together when its creator runs
   again (spec §10.1, Generations), which is what regions are for: everything in a region is freed at
   once, in constant time [8]. Counting the references into each region keeps that safe when a
   member escapes, by deleting a region only once nothing points into it [9]. Leptos ties its signals' lives to an ownership tree
   over an arena [10]; lithia has no owner hierarchy, but generations are one in all but name.

### Decide support without tracing

7. **Counted support, traced only for cycles.** Most of the support graph is a DAG: parents to
   children, creators to generations. A count per node of the supported nodes and anchors that reach it
   decides support for those by plain counting: a hub's drop decrements 2 000 counts and lets go of
   each node whose count reaches zero, with no lock, on as many threads as drop hubs. Only write cycles
   and self-support need tracing, for the nodes in a strongly connected component. That is how CPython
   collects (reference counting first; its cycle collector only tracks containers [11]), how Bacon and
   Rajan's collector treats acyclic objects [1], and what LXR does for Java, with 6 times the throughput
   and 30 times lower 99.9th-percentile latency than Shenandoah on Lucene [12]. Strongly connected
   components can be maintained as edges are added [13]. The hard part is the same one the sweep lock
   solves today: a count rising again while another thread lets go below it, which needs a model
   before code, as approach 1 of `locking.md` had.
8. **Parallel sweeps.** If tracing stays, one large sweep can be shared out: parallel collectors split
   marking across threads with work-stealing queues, at 4 to 5.5 times on 8 processors [14]. Two sweeps
   over disjoint regions could run at once with per-node marks claimed by compare-and-swap, a sweep that
   meets another's mark merging into it or waiting.
9. **Thread-local nodes.** Thread-local heaps collect objects no other thread can see without
   synchronising, and halved collection time for Java; a write barrier marks an object global when it
   escapes [15]. A node no other thread has touched could be swept under no shared lock. In Rust a handle
   can move between threads with no hook to see it, so this needs a barrier on every handle operation;
   speculative.

### Create without a shared counter

10. **Ids from logical clocks.** Each creation takes the next `NodeId` from one process-wide counter,
    one cache line every creating thread writes (`counter` row). The only order ids must keep is
    parents before children (spec §5.1), and a child is always created after its parents. A Lamport
    clock gives exactly that without a shared counter: a thread's next id is one more than the largest
    of its own last id and its parents' ids, ties broken by thread [16]. Jane Street's Incremental orders
    by height instead, each node's height above that of every node it reads [17]. Either keeps the topological order;
    what else reads ids as a global creation order needs checking first.
11. **Per-thread diagnostic counts.** Each site counts its live nodes for the growth warning (12) in one
    atomic, which every thread creating at that site writes; per-thread shards summed on read cost
    nothing to the warning.
12. **Hot hubs.** Effects created on one shared hub each take its scheduling lock to subscribe
    (`create-shared` row). Children a run creates could subscribe in one hold, as a batch.

## Compared

| Approach | Helps | Risk | Changes behaviour |
| --- | --- | --- | --- |
| 1. In-place drops | freeing | low | no |
| 2. Marks, not maps | sweeps | low | no |
| 3. Fewer allocations | creation, freeing | low | no |
| 4. Drop threads | freeing on many threads | low | no |
| 5. Allocator, node pool | creation, freeing | low (pool: medium) | no |
| 6. Generation regions | rebuilt children | medium | no |
| 7. Counted support | sweeps, without a lock | high, model first | no |
| 8. Parallel sweeps | large sweeps | high | no |
| 9. Thread-local nodes | sweeps | high, speculative | no |
| 10. Logical-clock ids | creation | medium | ids are no longer in creation order |
| 11. Sharded site counts | creation | low | no |
| 12. Batched subscription | creation on hot hubs | medium | no |

## A plan

1. Measure on the 7950X3D: the lock's figures, pinning, and mimalloc (`BENCHMARKS.md`). Together they
   say whether the lock's work, the allocator or lithia's per-node work dominates.
2. The low-risk constant factors, each with the benchmark before and after: 4 (no thread starts), 1
   (in-place body drops), 11 (sharded site counts), 2 (marks), 3.
3. If the allocator dominates on Windows: a recommendation in the README, then 5's node pool or 6.
4. If the sweep lock's serial work dominates: 7, modelled in `Lifetimes.tla` first; 8 only if 7 does
   not fit.
5. If the shared counter shows: 10.

## Sources

1. D. F. Bacon, V. T. Rajan, [Concurrent Cycle Collection in Reference Counted
   Systems](https://pages.cs.wisc.edu/%7Ecymen/misc/interests/Bacon01Concurrent.pdf), ECOOP 2001.
2. tokio, [runtime/blocking/pool.rs](https://docs.rs/crate/tokio/1.52.0/source/src/runtime/blocking/pool.rs).
3. P. Liétar et al., [snmalloc: A Message Passing
   Allocator](https://www.microsoft.com/en-us/research/uploads/prod/2020/04/snmalloc.pdf), ISMM 2019.
4. A. Turon, [Lock-freedom without garbage collection](https://aturon.github.io/tech/2015/08/27/epoch),
   on crossbeam's epochs.
5. [RFC: Replacing the default CRT allocator on
   Windows](https://lists.llvm.org/pipermail/cfe-dev/2020-July/066178.html), LLVM, 2020.
6. D. Leijen, B. Zorn, L. de Moura, [Mimalloc: Free List Sharding in
   Action](https://www.microsoft.com/en-us/research/publication/mimalloc-free-list-sharding-in-action), APLAS 2019.
7. [sharded-slab](https://docs.rs/sharded-slab/latest).
8. [Region-based memory management](https://en.wikipedia.org/wiki/Region-based_memory_management); M. Tofte,
   J.-P. Talpin, POPL 1994.
9. D. Gay, A. Aiken, [Memory management with explicit
   regions](https://www.academia.edu/175363944/Memory_management_with_explicit_regions), PLDI 1998.
10. Leptos, [The life cycle of a signal](https://book.leptos.dev/appendix_life_cycle.html), and
    [reactive_graph's arena](https://docs.rs/crate/reactive_graph/0.2.4/source/src/owner/arena.rs).
11. CPython, [garbage collector design](https://chromium.googlesource.com/external/github.com/python/cpython/+/fb8d8d9c9f9cbc94fa58887e13e63bf33fe0caac/InternalDocs/garbage_collector.md).
12. W. Zhao, S. M. Blackburn, K. S. McKinley, [Low-Latency, High-Throughput Garbage
    Collection](https://arxiv.org/abs/2210.17175v1), PLDI 2022.
13. B. Haeupler et al., [Incremental Cycle Detection, Topological Ordering, and Strong Component
    Maintenance](https://arxiv.org/abs/1105.2397), 2011.
14. C. Flood, D. Detlefs, N. Shavit, X. Zhang, [Parallel Garbage Collection for Shared Memory
    Multiprocessors](https://static.usenix.org/events/jvm01/full_papers/flood/flood.pdf), JVM 2001.
15. T. Domani et al., [Thread-local heaps for Java](https://www.academia.edu/24055101/Thread_local_heaps_for_Java),
    ISMM 2002.
16. [Lamport timestamps](https://en.wikipedia.org/wiki/Lamport_clock); L. Lamport, Time, Clocks, and the
    Ordering of Events in a Distributed System, 1978.
17. Jane Street, [Incremental's node.ml](https://ocaml.org/p/incremental/v0.15.0/doc/src/incremental/node.ml.html).
