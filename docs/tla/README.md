<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# TLA+ models

Two specifications, checked by TLC on the small graphs in [`models/`](models), are the TLA+ specification
design spec §11.4 step 8 asks for:

- [`Scheduling.tla`](Scheduling.tla): the scheduling protocol (§7.2–7.4, §11.4);
- [`Lifetimes.tla`](Lifetimes.tla): node lifetimes, support and sweeps (§10.1–10.2).

Each step of either is one lock section or one atomic operation of the engine. TLC checks the models
on each push and pull request (`ci.yml`), and the two largest, `MCSharedTarget` and `MCDiamond`, before
a nightly release is promoted (`extended.yml`), so a model that fails blocks a release.

## Scheduling

### What it models

- outside writers, each delivering one batch as a root epoch;
- run tasks (`Slots` per node): starting a run, polling its body (which may write its targets, change its
  value or not, and suspend to poll again), the poll's delivery, releasing or handing over its epochs,
  supersession, protection of a replacement run, and the fast path's task boundaries;
- cut reruns, started when their epoch settles;
- framing, releases, deciding and settling entries, counted waits, and the epoch tree's token
  accounting.

A process's pending frame visits and releases are sets, handled in every order: a superset of the
engine's. Bodies are abstract: a run writes any subset of its targets, freely while the clock stays
below `MaxClock`, and changes its value or not; a node outside `Equal` always changes.

Not modelled: node creation and lifetimes (in `Lifetimes.tla`), creator order, fragility, reflow,
generators, `WHOLE_EPOCHS` and disposal. The loom models and the edge tests cover those.

### Properties

| Property | Meaning |
| --- | --- |
| `TokensConserved` | Every open token in an epoch is held by exactly one entry, run, handover or pending close. |
| `TreeCounts` | Each epoch's `sub` is its own count plus its child epochs with open work; the scope counts root epochs with open work. |
| `NoStuckWait`, `WaitsCounted` | A child waits on a parent only for releases that parent still owes it or has under way. |
| `ObligationsHeld` | An unreleased obligation holds a token, so its epoch cannot settle before the release. |
| `ScopeConsistent` | When the scope's count is zero (`flush` returns), every node's last run started after its parents' last changes, except changes not yet delivered. |
| `NoGlitch` | Without writes from bodies, no run starts on an entry while a parent still owes the epoch. |
| `Terminates` | Under weak fairness, every behaviour reaches a state with no work left. |

### Models

| Model | Graph and load |
| --- | --- |
| `MCDiamond` | A diamond under two outside writes: glitch freedom, supersession and handover. One side may compute an equal value. |
| `MCSupersede` | A chain of synchronous nodes under two outside writes. |
| `MCAsyncChain` | The same with an async middle node that polls twice: child epochs and protected runs. |
| `MCSharedTarget` | Two runs serving one epoch write the same source, whose region is a chain. |
| `MCWriteBack` | A body writes its own parent beside a join (Case P): cuts and cut reruns. |
| `MCBinding` | A two-way binding of two sources: a write cycle through two writers. |

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

## Lifetimes

### What it models

- outside handles on each thread, cloned, sent to another thread and dropped, with each node's atomic
  anchor count; the last drop raises a starting point;
- internal handles: a body's captures, which a body may clone into an outside handle, and the handles a
  block took over, which count as outside handles once its lease ends;
- creation in three steps: the node linked with its handle, then its creator's generation and support's
  self-reference, then its writer edges;
- a creator's next run releasing its generation, and `.scoped()`;
- freeing as a step of its own once nothing holds a node: its targets and generation become starting
  points, and its body, with what it captured, is dropped later;
- sweeps under the sweep lock, with their exact checks that may give up, the region, the trust of held
  nodes outside it, and the references they hold until after the lock; walks that hold again what support
  let go of, when a count rises from zero and when a writer registers.

A sweep reads anchor counts while other threads drop handles, so it may count any anchor that fell
during it. Not modelled: values and scheduling, keyed nodes and holdovers (which support treats like a
generation), disposal and ghosts. The loom models and the edge tests cover those.

### Properties

| Property | Meaning |
| --- | --- |
| `CountsExact` | Every anchor count is the number of outside handles. |
| `RefsHeld` | A node's parents, generation and captures live while it does. |
| `LetGoSound` | Support never lets go of a node an anchor reaches. |
| `NoLeak` | Once lithia has nothing left to do, support holds only nodes an anchor reaches. |
| `SupportHolds` | Once lithia has nothing left to do, support holds every unscoped top-level node with parents that an anchor reaches. |
| `Terminates` | Under weak fairness, every behaviour reaches a state with nothing left to do. |

### Models

| Model | Graph and load |
| --- | --- |
| `MCReturn` | An effect `e` on `a` and its child on `e` and `y`: handles move between threads, bodies clone what they captured, and a block's lease ends. |
| `MCWriter` | An effect on `x` kept alive by its child; a writer of `x` is created. |
| `MCGeneration` | An effect creates a node in its run, a top-level effect on that node is created, and the creator's next run releases it. |
| `MCScoped` | A scoped effect, its dependent, and a writer whose body keeps the dependent alive. |

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

## Running

[`tlc.sh`](tlc.sh) fetches the TLA+ tools' current release (v1.8.0; v1.7.4 takes hours over
`Lifetimes.tla`), prints its version and runs TLC on one model; it needs Java 11 or newer. Running several
models at once needs `-Xmx` per process, since each otherwise sizes its heap for the whole machine.

```sh
cd docs/tla/models
../tlc.sh MCReturn
```
