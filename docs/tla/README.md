<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# TLA+ models

Two TLA+ specifications of the engine, checked by TLC on the small graphs in [`models/`](models):

- [`Scheduling.tla`](Scheduling.tla): the scheduling protocol (design spec §6.3, §7.2–7.4, §8, §9,
  §10.4, §11.3, §11.4);
- [`Lifetimes.tla`](Lifetimes.tla): node lifetimes, support, sweeps, keyed nodes and disposal
  (§10.1–10.5).

Each step is one lock section or one atomic operation of the engine. `ci.yml` checks the models on
each push and pull request. The three largest, `MCSharedTarget`, `MCWhole` and `MCDiamond`, run in
`extended.yml` before a nightly release is promoted, so a failing model blocks a release. `MCDiamond` needs about four
hours: it finished once in CI with 26 147 949 states at depth 109 and no error. Since runners differ in
speed, `extended.yml` stops it after 270 minutes and passes it if no error was found by then.

## Scheduling

### What it models

- outside writers, each delivering one batch as a root epoch: it writes the sources in it and
  reflows the derived nodes in it;
- run tasks (`Slots` per node): starting a run, polling its body (which may write or reflow its
  targets, change its value or not, suspend to poll again, yield an item as a generator, or panic),
  the poll's delivery, releasing or handing over its epochs, supersession, protection of a replacement
  run and its end at a generator's yield, and the fast path's task boundaries;
- cut reruns, started when their epoch settles;
- framing, seeding reflowed nodes, releases, deciding and settling entries, counted waits, and the
  epoch tree's token accounting;
- panics and resilience: only the current run fails its node, a child fails through its parents when
  its resilience gives way, when its entry becomes ready or its run would start, and recovers (rerun or
  restored) once they no longer fail it;
- `batches = WHOLE`: a node defers its run while another of its entries waits, up to a bound;
- creator order: a created node waits, registered with its creator, while the creator may still
  replace it, and is woken when the creator settles, starts or ends a run;
- disposal: ghosts under each one's write lock, their entries, handovers and runs retired, `disposed =
  FAIL` ghosts counted as failed parents, and the survivors' reruns;
- creation: a node made from outside, or by its creator's first poll, is linked to its parents and
  opens an entry, waiting on them, in each epoch a parent still owes; those entries wait for the
  creation's seed, delivered with the batch that made the node, which is its first run;
- keyed nodes: a run of their creator holds them back until its first poll reclaims them, maybe with
  new captures, which reruns them unless a run of theirs is owed already;
- async source blocks: a created source whose first run is its block; a write cancels that run, or
  keeps it from starting, and its late items are discarded;
- `defer`: a batch's deferred function holds a scope token, fires once the batch's epoch settles, runs
  as a writer of its own, and returns the token once its writes are delivered;
- runtime shutdown: from then on no task is polled again; a dropped task's run finalizes without a
  successor, and its node settles its runnable entries, closes its handover, fails, and tells the
  children of the epochs the run served.

A process's pending frame visits, seeds, releases and resumes are sets, handled in every order: a
superset of the engine's. Bodies are abstract: a run writes any subset of its targets, freely while the
clock stays within `MaxClock`, and changes its value or not; a node outside `Changing` never changes.

Not modelled: lifetimes (in `Lifetimes.tla`) and the rerun rate, a diagnostic that counts run starts
and warns without changing any step. A created node stays held by its creator, a node is created at
most once, and every run of a keyed node's creator reclaims it at its first poll. After a shutdown no
run starts on the fast path either, though a poll under way may still start one. The loom models and
the edge tests cover those.

### Properties

| Property | Meaning |
| --- | --- |
| `TokensConserved` | Every open token in an epoch is held by exactly one entry, run, handover or pending close. |
| `TreeCounts` | Each epoch's `sub` is its own count plus its child epochs with open work; the scope counts root epochs with open work. |
| `NoStuckWait`, `WaitsCounted` | A child waits on a parent only for releases that parent still owes it or has under way. |
| `ObligationsHeld` | An unreleased obligation holds a token, so its epoch cannot settle before the release. |
| `ScopeConsistent` | When the scope's count is zero (`flush` returns), every node's last run started after its parents' last changes, except changes not yet delivered, ghosts and nodes their parents fail. |
| `AbsorbedReleased` | Without writes from bodies or resilience, once a root epoch's subtree has no open work (its writer's `flush` returns), every child of a node it wrote or reflowed has released a run that started after it. |
| `NoGlitch` | Without writes from bodies, no run starts on an entry while a parent still owes the epoch. |
| `ResilienceConsistent` | When the scope's count is zero and no failure is still to be delivered, a node is failed through its parents exactly when its resilience gives way to their failures. |
| `WholeTogether` | A `batches = WHOLE` node's run, started below the bound, leaves no entry of it waiting. |
| `CreatorFirst` | A created node's run never starts while its creator may still replace or keep it. |
| `ReflowRan` | When the scope's count is zero, every reflowed node has run since its last reflow. |
| `YieldsHeld` | A run holds its epochs open until a poll that does not yield, so `flush` waits for a generator's items. |
| `GhostsRetired` | A retired ghost never has an entry, a handover, a run or a queued run again. |
| `SurvivorsRerun` | When the scope's count is zero, every survivor of a disposal has run since. |
| `SeededFirst` | A created node never starts a run before its creation is delivered. |
| `FirstRuns` | When the scope's count is zero, every created node whose creation is delivered has run, unless its parents fail it or a write stopped it, as a block, before it started. |
| `RefreshRan` | When the scope's count is zero, every keyed node reclaimed with new captures has run since. |
| `BlocksStop` | A block a write stopped has no run in flight that may still publish. |
| `DeferAfterChanges` | A deferred function fires only once every child of a node its batch wrote has released a run started after the write. |
| `DefersRan` | When the scope's count is zero, every deferred function has run and delivered its writes. |
| `Terminates` | Under weak fairness, every behaviour reaches a state with no work left. |

### Models

| Model | Graph and load | States |
| --- | --- | --: |
| `MCDiamond` | A diamond under two outside writes: glitch freedom, supersession and handover. One side never changes its value. | `extended.yml` |
| `MCSupersede` | A chain of synchronous nodes under two outside writes. | 265 525 |
| `MCAsyncChain` | The same with an async middle node that polls twice: child epochs and protected runs. | 1 351 417 |
| `MCSharedTarget` | Two runs serving one epoch write the same source, whose region is a chain. | `extended.yml` |
| `MCWriteBack` | A body writes its own parent beside a join (Case P): cuts and cut reruns. | 2 645 |
| `MCBinding` | A two-way binding of two sources: a write cycle through two writers. | 1 145 |
| `MCAbsorb` | Two writers, each writing one parent of a join: a run started for one write reads the other before that write's epoch reaches it. | 265 525 |
| `MCRecover` | An async node whose value changes and that may panic at any poll, under two writes: its child (`resilience = NONE`) fails while it is failed, and reruns once it recovers with a new value. | 784 405 |
| `MCResilience` | The same with a grandchild under `resilience = PARTIAL` on both, and values that never change, so a recovered child is restored without a run. | 101 481 |
| `MCWhole` | A `batches = WHOLE` join of a source and an async node on it, under two writes: an epoch that becomes ready while the other still waits is deferred, and the run takes both. | 4 278 523 |
| `MCGenerator` | A generator on a source, under two writes: an item keeps the run's epochs open until it ends, and a yield ends a replacement run's protection. | 361 921 |
| `MCCreator` | A source read by a creator and by the node its run created, under two writes: the created node waits while its creator may still replace it. | 673 071 |
| `MCReflow` | A write to a source and a reflow of the async node below it: the node reruns, a run already under way is waited for, and the child sees the reflow as a change. | 1 657 084 |
| `MCReflowBack` | A child whose body reflows its parent (Case P with a reflow): the parent reruns on the child's chain, and the child, cut, reruns once the epoch settles. | 1 278 |
| `MCDispose` | Disposing of a node a write reaches: its run is torn down, the child it fails (`disposed = FAIL`, `resilience = NONE`) fails, and the other survivor reruns. | 933 498 |
| `MCCreate` | A node created from outside below an async node a write reaches (Case N): its entries in the epochs its parent still owes wait for its seed, and it runs its first run. | 183 924 |
| `MCCreateRun` | A node its creator's run creates, reading the creator, under two writes: it is seeded in the epochs the run serves, waits on the creator there, and starts once decided. | 226 219 |
| `MCKeyedRerun` | A keyed node without parents that its creator's runs reclaim, under two writes: it waits until reclaimed, and a reclaim with new captures reruns it, also while a run of its is under way. | 139 861 |
| `MCBlock` | An async source block, created from outside, and its child, under a write: the write stops the block, before it starts or while it runs, and its late items are discarded. | 54 156 |
| `MCDefer` | A writer of a source an async node reads defers a function that writes the source again: the function runs once the writer's epoch has settled, and `flush` waits for it. | 935 |
| `MCShutdown` | An async node and its child (`resilience = NONE`) under a write while the runtime shuts down: dropped tasks give their nodes up, which fail, their children fail with them, and `flush` returns. | 1 718 |

States are the distinct states TLC finds once it has explored every behaviour. `MCDiamond` and
`MCSharedTarget` are too large to explore outside `extended.yml`.

### Findings

TLC's first runs found two races in v0.4.0.1, and the loom model written for the first exposed a third.
Each is reproduced in the engine by a loom model and fixed in v0.4.0.2:

1. Two runs serving one epoch that wrote the same source each released its children, through one wait
   per child. One delivery's release could reach a child while the other still framed the child's
   region, so a node below waited forever and the epoch never settled.
2. A run superseded after publishing its value dropped the value's stamp, so its children never reran
   if the run replacing it computed an equal value. The successor now releases with that stamp.
3. A parent framed again in an epoch while its earlier release there was still under way gave its
   child two releases for one wait; the one carrying no change could arrive first and settle the child,
   which then never saw the change.

The fix for the first and third: a child counts the releases each parent owes it, and each release
reaches exactly the children its framing counted. For the second, the run that takes over a superseded
run's epochs releases with the stamp of the item it published. Undoing either fix in the model fails it
again: waits that do not count break `NoStuckWait` on `MCSharedTarget`, and dropping the successor's
stamp breaks `ScopeConsistent` on `MCSupersede`.

The architectural review of 2026-11-22 found a fourth, which the model missed because `ScopeConsistent`
checks only that runs started: a run started after a write, but before the write's epoch reached its
node, absorbed the change, so the write's entry settled unchanged and its epoch could settle, `flush`
returning, before the run published. `AbsorbedReleased` and `MCAbsorb` catch it (2 937 states, 3
seconds). The fix in v0.5.0: such an entry joins the run in flight while that run has not released, and
otherwise settles with the node's current version. With it, `MCAbsorb` passes: 265 525 states, depth 77,
3 minutes.

Extending the model to generators, `batches = WHOLE`, creator order, resilience, reflows and disposal
(2026-11-25) found three more faults in v0.5.0's engine, each fixed with a test that fails without it:

1. A run that failed after it had released its epochs, in a later poll of an async body or after a
   generator's item, put its node into `Failed`, but nothing told its children: one under
   `resilience = NONE` or `PARTIAL` stayed `Ready` until the next change reached it.
   `ResilienceConsistent` fails on `MCRecover` in 28 seconds. Such a run now tells its children through
   child epochs, as an item that changed nothing would. Edge tests:
   `a_failure_after_the_first_suspension_reaches_strict_children` and
   `a_generator_failing_after_an_item_fails_its_strict_children`.
2. The absorbed change of the fourth finding had a second form: when the run that read it was
   superseded and had handed its epochs over before the write's entry was decided, the entry settled,
   and `flush` could return before any value that includes the write. `AbsorbedReleased` fails on
   `MCWhole`. The entry now joins the handover, and the successor releases it. A whole node whose run
   waits behind an epoch also starts it once that epoch joins, or settles with only a handover left; it
   could otherwise wait forever. Edge test: `flush_waits_for_the_successor_of_the_run_that_absorbed_its_write`.
3. A node disposed of while a write's framing walk was on its way below it released its surviving
   children when it was retired, before the walk reached them; the walk then made them wait on the
   ghost, which never released again, so the epoch never settled. `NoStuckWait` fails on `MCDispose` in
   3 seconds. A walk now opens no entry below a ghost, whose survivors rerun anyway. Loom model:
   `a_write_frames_through_a_node_being_disposed`.

Undoing each feature's rule in the model fails it, on two shared cores here:

| Undone | Fails | Model | Time |
| --- | --- | --- | --- |
| A late failure tells the children | `ResilienceConsistent` | `MCRecover` | 28 s |
| An absorbed change joins a handover | `AbsorbedReleased` | `MCWhole` | 27 min |
| A walk skips a ghost parent | `NoStuckWait` | `MCDispose` | 3 s |
| Survivors rerun | `ResilienceConsistent` | `MCDispose` | 4 s |
| A whole node's run waits while an entry of it does | `WholeTogether` | `MCWhole` | 16 s |
| A yield keeps the run's tokens | `YieldsHeld` | `MCGenerator` | 11 s |
| A gated node does not start | `CreatorFirst` | `MCCreator` | 5 s |
| Settling, joining and finishing wake gated nodes | `Terminates` | `MCCreator` | 10 min |
| A reflow makes its node run | `ReflowRan` | `MCReflowBack` | 5 s |

Extending the model to creation, keyed reruns, async source blocks, `defer` and shutdown (2026-11-27)
found one more fault in v0.5.0's engine: a run task dropped because its runtime shut down failed its
node, but told none of its children, which had been released already, at the run's first poll or as it
finalized: one under `resilience = NONE` or `PARTIAL` stayed `Ready` beside its failed parent.
`ResilienceConsistent` fails on `MCShutdown` in 46 seconds. The node now tells the children of the
epochs the dropped run served through child epochs, as a run failing after its release does. Edge
test: `a_runtime_shutting_down_fails_the_strict_children_of_its_runs`; kept mutation W8.

Undoing each new rule fails the model:

| Undone | Fails | Model | Time |
| --- | --- | --- | --- |
| A shutdown tells the children of the dropped run's epochs | `ResilienceConsistent` | `MCShutdown` | 7 s |
| A node decides nothing before its seed | `SeededFirst` | `MCCreate` | 9 s |
| Linking opens entries in the epochs a parent still owes | `ScopeConsistent` | `MCCreate` | 2.5 min |
| A poll's creations are delivered with its batch | `Terminates` | `MCCreateRun` | 31 s |
| A keyed node waits until its creator's run reclaims it | `CreatorFirst` | `MCKeyedRerun` | 33 s |
| A reclaim with new captures reruns its node | `RefreshRan` | `MCKeyedRerun` | 7 s |
| A write cancels the block's run | `BlocksStop` | `MCBlock` | 3 s |
| A run start closes a stopped block's epochs | `BlocksStop` | `MCBlock` | 4 s |
| A deferred function fires once its epoch has settled | `DeferAfterChanges` | `MCDefer` | 2 s |
| A deferred function holds a scope token until its writes are delivered | `DefersRan` | `MCDefer` | 3 s |

## Lifetimes

### What it models

- outside handles on each thread, cloned, sent to another thread and dropped, with each node's atomic
  anchor count; the last drop raises a starting point;
- internal handles: a body's captures, which a body may clone into an outside handle, and the handles a
  block took over, which count as outside handles once its lease ends;
- creation in three steps: the node linked with its handle, then its creator's generation and support's
  self-reference, then its writer edges;
- a creator's next run releasing its generation, while its keyed members wait in the holdover, still
  held, until the run reclaims them or ends and releases them; and `.scoped()`;
- freeing as a step of its own once nothing holds a node: its targets and generation become starting
  points, and its body, with what it captured, is dropped later;
- sweeps under the sweep lock, with their exact checks that may give up, the region, the trust of held
  nodes outside it, and the references they hold until after the lock; walks that hold again what support
  let go of, when a count rises from zero and when a writer registers;
- with `FastRise`, rises from zero without the lock: the count rises in one step, marked rising, and the
  rise ends in another, taking the lock and walking if a sweep holds it or support has let go of a node
  that still lives; a sweep may read as zero any count that was zero while it ran, though not the count
  of a node created meanwhile, which starts at one; and a handle added onto a count still rising takes
  the lock. With `SkipSupported`, a count that falls to zero raises no
  starting point while a parent anchors and no node has been scoped. Both are off in the models above
  them in the table, which keep their counts, and on in the others;
- disposal: the freed branch becomes ghosts, which are never anchors, then is torn down in one
  operation: support's references, bodies and generations are released, the ghosts leave their parents,
  and their members, targets and surviving descendants are swept once it ends; a node created with only
  torn-down ghosts for parents is disposed of at once, and one created under a torn-down creator is held
  by neither its creator nor support; a node linked to a ghost after the walk, before the ghost was torn
  down, is handled by that teardown: disposed of with it if all its parents are ghosts, swept as a
  survivor otherwise.

A sweep reads anchor counts while other threads drop handles, so it may count any anchor that fell
during it. Not modelled: values and scheduling (in `Scheduling.tla`). The loom models and the edge
tests cover those.

### Properties

| Property | Meaning |
| --- | --- |
| `CountsExact` | Every anchor count is the number of outside handles. |
| `RefsHeld` | A node's parents, generation and captures live while it does, until disposal tears it down. |
| `LetGoSound` | Support never lets go of a node an anchor reaches, unless it is scoped or disposed of. |
| `NoLeak` | Once lithia has nothing left to do, support holds only nodes an anchor reaches. |
| `SupportHolds` | Once lithia has nothing left to do, support holds every unscoped top-level node with parents that an anchor reaches, except ghosts. |
| `GenerationsExact` | A node is in its creator's generation exactly when it names that creator, and a holdover holds only keyed members of it. |
| `GhostsReleased` | A torn-down ghost holds nothing, and support never holds it. |
| `OrphansDisposed` | Once lithia has nothing left to do, a node whose parents are all ghosts is a ghost too. |
| `Terminates` | Under weak fairness, every behaviour reaches a state with nothing left to do. |

### Models

| Model | Graph and load | States |
| --- | --- | --: |
| `MCReturn` | An effect `e` on `a` and its child on `e` and `y`: handles move between threads, bodies clone what they captured, and a block's lease ends. | 23 240 |
| `MCWriter` | An effect on `x` kept alive by its child; a writer of `x` is created. | 66 731 |
| `MCGeneration` | An effect creates a node in its run, a top-level effect on that node is created, and the creator's next run releases it. | 78 358 |
| `MCScoped` | A scoped effect, its dependent, and a writer whose body keeps the dependent alive. | 77 166 |
| `MCKeyed` | An effect creates a keyed source in its run, and a top-level effect on it is created, which only the creation edge supports; the creator's next run holds the source over, then reclaims it or ends and releases it. | 53 870 |
| `MCDisposal` | Sources `a` and `b`, an effect `e` on both and a child `d` of `e`, created later: disposing of `b` leaves `e` a survivor that `a` alone supports; disposing of `e` takes `d` with it, or `d` is created with only a ghost for a parent. | 71 693 |
| `MCLateChild` | Sources `a` and `b`, and effects `c` on `a` and `d` on both, created while `a`'s disposal is under way: linked after the walk, before `a` closed, `c` is disposed of and `d` survives; linked after `a` closed, a child disposes of itself. | 13 842 |
| `MCFastRise` | A source `a` and an effect `e` on it that only support holds: `a`'s last handle drops while two threads clone `a` from `e`'s body, one rising from zero without the lock and one adding onto that rise. | 1 211 |
| `MCSkip` | A source `a`, an effect `p` on it and an effect `m` on `p`: `a`'s and `m`'s last handles drop on two threads, so `m`'s fall may skip its sweep while `a`'s sweep lets go of `p`. | 2 840 |

### Findings

The model found five faults in v0.4.0.2's lifetimes, fixed in v0.5.0 (spec §10.1):

1. A block's lease ended before its handles counted as outside handles. A leased handle dropped on
   another thread in between took an anchor off a count that did not include it: a sweep could take the
   wrapped count for an anchor, and the rise that followed went unnoticed. The handles now count before
   the lease ends.
2. A count that rose from zero inside an operation walked downstream only when the operation ended. A
   handle dropped on another thread in between lowered the count again and swept; the walk that followed
   held again nodes no anchor reached, which leaked. A rise now happens under the sweep lock, with its
   walk.
3. A sweep could miss a rise that happened while it ran, and let go of a node the new anchor reached.
   Rises under the sweep lock rule that out.
4. A walk from a scoped node stopped at it, so a scoped node that was an anchor again did not hold
   again what depended on it. A walk now always continues past the node it starts from.
5. A writer created after support let go of a node downstream of its targets did not hold it again, so
   the node was freed once its other holder went, while the writer still wrote its input. A new writer
   now walks too.

Undoing each fix in the model fails it again: the lease order breaks `LetGoSound` on `MCReturn`, the
deferred walk breaks `LetGoSound` and `NoLeak` on `MCReturn`, a walk that stops at its starting node
breaks `SupportHolds` on `MCScoped`, no walk when a writer registers breaks `SupportHolds` on `MCWriter`,
no walk at a rise breaks `SupportHolds` on `MCReturn`, and trusting held nodes inside the region breaks
`NoLeak` on `MCReturn`. In the engine, the loom model
`a_moved_out_handle_drops_while_its_lease_ends_and_support_has_let_go` and the edge tests
`a_scoped_node_that_is_an_anchor_again_holds_what_depends_on_it` and
`a_new_writer_holds_again_what_support_let_go_of` fail on v0.4.0.2 and pass now.

Extending the model to keyed nodes, holdovers and disposal (2026-11-25) found one more: a node created
as a child of a node being disposed could be freed with it and torn down before its creation stored
support's reference to it, so support held a ghost until its last handle dropped. `GhostsReleased`
fails on `MCDisposal` in 13 seconds. Creation now stores the reference under the lock teardown takes it
with, and only for a node that is not a ghost.

Undoing each rule of the new parts fails the model:

| Undone | Fails | Model | Time |
| --- | --- | --- | --- |
| Creation holds no ghost | `GhostsReleased` | `MCDisposal` | 13 s |
| A ghost is never an anchor | `NoLeak` | `MCDisposal` | 5 min |
| Teardown sweeps the survivors | `NoLeak` | `MCDisposal` | 35 s |
| A run's end sweeps the holdover it releases | `NoLeak` | `MCKeyed` | 4 min |

Extending it to children linked to a node while disposal tears it down (2026-11-27) found no fault:
the engine's teardown already hands back the children linked after its walk. Undoing that fails the
model:

| Undone | Fails | Model | Time |
| --- | --- | --- | --- |
| A teardown disposes of a late child whose parents are all ghosts | `OrphansDisposed` | `MCLateChild` | 5 s |

Taking the sweep lock out of rises and falls that change nothing (2026-11-28) was modelled before it was
written. Undoing each part of it fails the model:

| Undone | Fails | Model | Time |
| --- | --- | --- | --- |
| A rise without the lock waits for a sweep under way | `LetGoSound` | `MCFastRise` | 2 s |
| A handle added onto a rise not done takes the lock | `LetGoSound` | `MCFastRise` | 2 s |
| Only a parent that anchors lets a fall skip its sweep, not one support holds | `NoLeak` | `MCSkip` | 4 s |
| No fall skips its sweep once a node has been scoped | `NoLeak` | `MCScopedFast` | 12 s |

## Running

[`tlc.sh`](tlc.sh) fetches the TLA+ tools' current release (v1.8.0; v1.7.4 takes hours over
`Lifetimes.tla`), prints its version and runs TLC on one model; it needs Java 11 or newer. Running several
models at once needs `-Xmx` per process, since each otherwise sizes its heap for the whole machine.

```sh
cd docs/tla/models
../tlc.sh MCReturn
```

CI runs each model through [`.github/scripts/tlc.py`](../../.github/scripts/tlc.py), which also checks its
distinct states and depth against [`models/counts.tsv`](models/counts.tsv): a change to a spec or a model
that moves a count fails until `tlc.py MODEL --record` records the new figures, in the same commit and
with the reason. `MCDiamond` is recorded as bounded: TLC explores it for 270 minutes.
