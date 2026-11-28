<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# Verification

How lithia is checked and where each check lives. [`CONTRIBUTING.md`](../CONTRIBUTING.md) has the
commands to run them, and [`known_bugs.md`](known_bugs.md) lists what they have not ruled out.

| Check | Where | Runs |
| --- | --- | --- |
| Validation cases A–BW (spec §13) | `crates/validation/tests` | `ci.yml`, on every platform |
| Compile errors (spec §4.6), one case per row; `node!`'s expansions, one snapshot per body form | `crates/validation/tests/ui`; `crates/macro/snapshots` | `ci.yml` |
| Edge-case tests, one module per area, and the random differential test | `crates/test/tests/edge` | `ci.yml` |
| Loom models of the real engine, one module per area | `crates/test/tests/loom` | `ci.yml` at bound 3, `extended.yml` at bound 4 |
| TLA+ models of scheduling and lifetimes, each against its recorded state count | `docs/tla` | `ci.yml`; the three largest in `extended.yml`, `MCDiamond` for 270 minutes |
| Kept mutations, cargo-mutants | `.github/scripts/mutations.py`, `.github/scripts/mutants.py` | `extended.yml`; cargo-mutants weekly, or on given files by a manual run |
| Stress runs of every test binary | `.github/scripts/stress.sh` | `extended.yml`, on Linux x86-64 and ARM and macOS |
| Miri | `extended.yml` | unit tests, validation cases and edge-case tests |

## Random differential test

`random_graphs_settle_on_their_definition_and_free_everything` builds random graphs over a few
sources: sync, async, `batches = WHOLE`, creator, keyed-creator and writer nodes. Three lithia tasks write
at once. When everything has settled, the test checks that every node's last run, and every creator's
child, computed its definition from the current values. It then drops the handles of nodes nothing
depends on, checks that support keeps them running, and finally checks that every node and body is
freed. 24 graphs run by default; `LITHIA_RANDOM_SEEDS=<n>` runs more and `LITHIA_RANDOM_SEED=<seed>`
replays one.

## TLA+

[`docs/tla/README.md`](tla/README.md) describes both specifications, their properties and models, what
TLC found and how to run it.

## Loom

Built with `RUSTFLAGS="--cfg lithia_loom"`, the engine swaps its mutexes, atomics, thread-locals, value
cells and version clock for loom's (`crates/lithia/src/sync.rs`) and runs bodies on a small model
executor instead of tokio. The cfg is not called `loom` because tokio reacts to that name. Each model
runs the real engine: the main thread delivers batches and polls runs, worker threads poll runs beside
it, and the model checks the final values once everything settles. Lifetime models also check each
scope's live count.

- Models: 52, in `crates/test/tests/loom/`. Each one's doc comment says what it races and which case
  it covers.
- Paths: every model runs with the fast path and again with `LITHIA_LOOM_FAST_PATH=off`, which gives
  every ready run a task of its own.
- Bound: 3 in `ci.yml`, a minute or two on two threads, and 4 in `extended.yml`.
  `LOOM_MAX_PREEMPTIONS` sets it.
- Determinism: sweeps keep their maps ordered by `NodeId`, loom builds always take the full path for
  scoped nodes, and tokens that close together close in epoch order. Models running side by side in
  one process therefore explore the same interleavings.
- Wakers: a pending task is parked until its waker moves it back to the queue. A thread waiting for a
  scope fails the model when every task stays parked while nothing else is under way, which is what a
  lost wakeup looks like.
- What loom sees: `crates/lithia/clippy.toml` forbids std's and parking_lot's locks, atomics and lazy
  cells outside `sync.rs`. A use loom need not see, such as an id counter, a diagnostics table or a
  handle's own flag, is allowed where it stands, with the reason beside it.
- Debug checks: every release of a scheduling lock checks its invariants (`Sched::check`), and epoch
  tokens dropped unclosed fail a debug build.
- Not modelled: reference counts (`Arc`, `Weak`) and tokio's internals. A race that lies entirely
  between two operations loom sees is collapsed. Every path that depends on a reference count is
  bracketed by locks or atomics loom does see, and stress runs check the rest. For example, the Case J
  model does not catch a run that closes its tokens while it still holds its node.

### Mutations applied by hand

Each of these made the named test fail. Newer mutations are kept as code (next section).

| Mutation | Fails |
| --- | --- |
| Counting captured handles as outside handles | Case AS |
| Letting support flow from a target back to a writer that has parents | Case J |
| Taking any incoming edge as support instead of the least fixpoint | Case AS |
| Generations as anchors instead of creation edges | Case AX |
| Trusting an "unsupported" answer at a sweep's border | Case AV |
| Skipping a starting point that lost its support | Cases J, AK |
| Writer edges for read-only captures | Cases AK, AY |
| Classifying a captured container as read-only by method name | the `items.get(0)` edge test |
| Letting an `Accessor` to a plain source node anchor | Case AZ |
| Letting support reach or pass a scoped node without handles, or supporting what depends on one | Case AT |
| Leaving a scoped node in its run's generation | Case BA |
| Holding only nodes with a body in a generation | Case BC |
| Holding a scheduling lock while copying a parent list | the resilience-decision model deadlocks |
| Taking a moved handle's count down before the edges exist | the `move` model |
| Checking starting points before taking the sweep lock, which reads counts outside it | the parent-and-child and zero-crossing models |
| Dropping a released generation, or the step's other strong references, after the step's spawns | the Case AO model |
| Releasing a superseded run's generation before it stops | the supersede model |
| Removing the `closed` check | Case AR |
| Never seeding a held creation under a closed creator | the Case AR model |
| Every reclaim counting as changed | Case BD |
| Releasing the holdover when a superseded run stops | Case BI, the keyed supersede model |
| Reclaiming regardless of parents | Case BF |
| Releasing keyed nodes with the generation instead of holding them over | Case BD, the holdover edge test |
| Ignoring the value type | the two-value-types edge test |
| Keeping a reclaimed node whose write targets changed | the write-target edge test |
| Comparing an `Arc` whose target has no `PartialEq` as opaque | Case BK |
| Not seeding a refreshed node to rerun | Cases BD, BH |
| Disposal leaving the holdover | the mid-run disposal edge test; the keyed disposal model reports a causality violation |
| Not disposing a node created with only ghosts for parents | Case BO, the only-parent creation model |
| Teardown not handing back the children linked when a node closes | the creation models |
| Not checking failed parents again at a run start | the one-parent creation model |
| Leaving the shape out of the reclaim conditions | Case BN |
| Keeping the first creation's comparator | Case BN |
| Checking an unread memo only at sweeps | the unread-memo edge tests, one per lost reader |
| `settled` not following write edges, or writes without one | Case BW, the write edge tests |
| Ending a lease only when its node goes | Case BP, the finished-block edge test |
| Ending a lease only when its block is dropped | the disposed-block edge test |
| A leased handle's drop ignoring the end of its lease | the lease model |
| Not holding again when an anchor returns | Case BQ |
| No creator gate | Case BR, the keyed creator and superseded-creation models |
| Running a released child that nothing can reach | the superseded-creation model |
| A refresh rerun beside a held-back run that runs the new body | the keyed creator model |
| Superseding a replacement before its item | Case BS |
| Keeping a replacement protected after its item | the generator supersession edge test |
| Forcing a rerun on every recovery from failed parents | Case BT |
| One lineage for all of a site's creations in a run | the sibling lineage edge test |
| Node ids instead of lineages in writer sets | Case S |
| No cut rerun | Case BU |
| A cut rerun that writes what it wrote | the suppression edge test |
| A cut rerun's writes to leave alone read when the cut is decided | the edge test of a cut rerun and writes recorded after the cut |
| `batches = WHOLE` not waiting | Case BV |
| Drop batches that never hand their drops on | the 20 000-node chain test |
| A freed child left in its parent's subscribers | the unread-memo edge tests |
| No waker for a run's first poll of a future | Case BI |
| A node's entries kept out of epoch order | the concurrent-writers edge test |
| A node written twice in one batch carrying its older stamp | the twice-written edge test |
| No delivery for a generator's later item when the poll wrote nothing else | the async source generator edge test |
| Writes to an async source node not stopping its block | Case AQ |
| A run start that keeps `Failed` | the parent-recovery and panicking-comparator edge tests |
| The fast path keeping runs of another runtime | the runtime edge test of bodies on their own runtime |
| Runs left past a task's budget dropped instead of handed on | the wide fan-out and drop edge tests |
| A runner task dropped before its poll dropping its runs | the edge test of runs packed for a runtime that shuts down |
| Tokens of one epoch closed together counted as one | the 20 000-node chain test |
| A parent framed again before its release arrived counted once | the two-writers model |
| A superseded run's published item not carried to its successor | the published-item model |

These make nothing fail, and are harmless:

- Dropping the walk's references under the sweep lock: the operation context already defers the
  sweeps those drops raise. The rule stays so that no teardown runs under the lock.
- Leaving the holdover out of support's edges: a sweep then skips what lies past a held-over node,
  which can only delay a let-go.
- Starting a `changes()` stream on a ghost without checking: the stream's first item finds the ghost
  and ends.
- Packing the share a task keeps into a runner task as well: the run goes to two tasks and still runs
  once, since a run takes its node's entries under the node's lock.
- A write's release reaching its node's subscribers at release time instead of those its framing
  walked: the two differ only for a node created between the two steps.

## Mutations

`.github/scripts/mutations.py` keeps each mutation as code, applies it to a copy of the workspace and
runs the test named for it, which must fail; `--list` names them. W1–W8 cover the protocol, F1–F5 the
fast path, S1–S3 support's walks, K1–K3 the wakes that end a stopped, retired or freed run, and Q1–Q3
the comparator a node takes by default. Two survive, both equivalent to the original: W3 releases a
write to a child linked after the write's framing, which waits on the written node only through an
entry, run or handover of that node that releases it as well, so the child reads the same value
whichever release comes first; F5 runs the kept share once either way.

### cargo-mutants

`extended.yml` runs cargo-mutants weekly, and on a manual run, over `crates/lithia` in 16 shards, with
the tests of `lithia`, `lithia-validation` and `lithia-test` but the compile-fail cases, and lists the
mutants they leave alive. `.cargo/mutants.toml` leaves out the mutants reviewed as unable to change
what lithia does, one pattern each, under its reason:

- compiled only for loom: the model executor and the loom twins in `sync.rs` and the fast path;
- the fast path's routing, which only chooses where a ready run is polled; the loom models check both routes;
- hash quality, buffers kept for reuse, token merging, duplicate spawns, and hints that only skip work;
- debug builds' invariant checks, the clean-up of a node being freed, and the safety nets for a
  protocol step a panic leaves undrained;
- internal ids' formatting, and replacements that compute the same value;
- a `Setter`'s capture hooks, failure records read only while their bit is set, unread-memo checks
  that find what they would anyway, a run's own waker, and `settled`'s trace pruning and wake-ups.

The first run, on ec53abb, left 406 of 1 584 mutants alive and 37 timed out. The review of 2026-11-27
mapped them onto 6041af0, with the 87 mutants new since or on changed lines: 518 in all. Of those,
123 are now caught, 77 by new or extended tests (each checked on the mutant itself) and 46 by tests
added since ec53abb; 33 time out and 33 do not build; 169 are left out as above; and 160 survive,
as `known_bugs.md` describes. One mutant found a bug: a `&'static` handle used as a dependency of a
`move` body was flagged as moved into it, so the nodes it could still write were let go.

## Stress

`.github/scripts/stress.sh <release runs> <debug runs> <one-core runs>` runs every test binary that
many times, in release, in debug and pinned to one core (Linux only), and keeps the output of every
failed run. A failure in a test listed in `.github/scripts/known-flaky.txt`, each described in
`known_bugs.md`, is reported without failing the run. `extended.yml` runs it 200, 100 and 100 times.

## Miri

`extended.yml` runs the unit tests and validation cases, and the edge-case tests in three shards by
module (`.github/scripts/miri-shards.py` lists them from the test files), under Miri, with isolation and leak checks off for tokio's threads. Several tests stage races
with sleeps, which Miri's slowness can reorder, so the job fails only on undefined behaviour or on an
operation Miri cannot run. In the last full run (27ba2d1) every shard finished within two hours, with
no undefined behaviour. Only the rerun-rate test failed: Miri cannot run a node 50 000 times a second.
