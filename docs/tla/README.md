<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# TLA+ models

Two TLA+ specifications of the engine, checked by TLC on the small graphs in [`models/`](models):

- [`Scheduling.tla`](Scheduling.tla): the scheduling protocol (design spec §7.2–7.4, §8, §9, §10.4,
  §11.4);
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
  FAIL` ghosts counted as failed parents, and the survivors' reruns.

A process's pending frame visits, seeds, releases and resumes are sets, handled in every order: a
superset of the engine's. Bodies are abstract: a run writes any subset of its targets, freely while the
clock stays within `MaxClock`, and changes its value or not; a node outside `Changing` never changes.

Not modelled: node creation and first runs, lifetimes (in `Lifetimes.tla`), keyed reclaiming and its
reruns, async source blocks and the writes that stop them, runtime shutdown, `defer` and the rerun
rate. A created node stays held by its creator. The loom models and the edge tests cover those.

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
- disposal: the freed branch becomes ghosts, which are never anchors, then is torn down in one
  operation: support's references, bodies and generations are released, the ghosts leave their parents,
  and their members, targets and surviving descendants are swept once it ends; a node created with only
  torn-down ghosts for parents is disposed of at once, and one created under a torn-down creator is held
  by neither its creator nor support.

A sweep reads anchor counts while other threads drop handles, so it may count any anchor that fell
during it. Not modelled: values, scheduling (in `Scheduling.tla`), and children linked to a node while
disposal tears it down. The loom models and the edge tests cover those.

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

## Running

[`tlc.sh`](tlc.sh) fetches the TLA+ tools' current release (v1.8.0; v1.7.4 takes hours over
`Lifetimes.tla`), prints its version and runs TLC on one model; it needs Java 11 or newer. Running several
models at once needs `-Xmx` per process, since each otherwise sizes its heap for the whole machine.

```sh
cd docs/tla/models
../tlc.sh MCReturn
```
