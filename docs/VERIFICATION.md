<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# Verification

What checks lithia, where each check lives and how to run it. The design spec (§11.4, step 8) names
the methods; this file holds their details. [`CONTRIBUTING.md`](../CONTRIBUTING.md) lists the commands,
and [`known_bugs.md`](known_bugs.md) what they have not ruled out.

| Check | Where | Runs |
| --- | --- | --- |
| Validation cases A–BW (spec §13) | `crates/validation/tests` | `ci.yml`, on every platform |
| Edge-case tests, one module per area, and the random differential test | `crates/test/tests/edge` | `ci.yml` |
| Loom models of the real engine | `crates/test/tests/loom.rs` | `ci.yml` at bound 3, `extended.yml` at bound 4 |
| TLA+ models of scheduling and lifetimes | `docs/tla` | `ci.yml`; the two largest in `extended.yml` |
| Kept mutations, cargo-mutants | `.github/scripts/mutations.py` | `extended.yml`; cargo-mutants weekly |
| Stress runs of every test binary | `.github/scripts/stress.sh` | `extended.yml`, on Linux x86-64 and ARM and macOS |
| Miri | `extended.yml` | unit tests, validation cases and edge-case tests |

## Random differential test

`random_graphs_settle_on_their_definition_and_free_everything` builds random graphs of sync, async,
`WHOLE_EPOCHS`, creator and keyed-creator nodes over a few sources, writes from three lithia tasks at
once, and checks once everything has settled that every node's last run, and every creator's child,
computed its definition from the current values. It then drops the handles of nodes nothing depends on,
checks that support keeps them running, and checks at the end that every node and body is freed. 24
graphs run by default; `LITHIA_RANDOM_SEEDS=<n>` runs more, `LITHIA_RANDOM_SEED=<seed>` replays one.

## TLA+

[`docs/tla/README.md`](tla/README.md) describes both specifications, their properties and models, what
TLC found and how to run it.

## Loom

Built with `RUSTFLAGS="--cfg lithia_loom"`, the engine swaps every mutex, atomic, thread-local, value cell and the version clock for loom's through one shim module, and runs bodies on a small model executor instead of tokio. The cfg is `lithia_loom` rather than `loom`, which tokio itself reacts to. The models run the real engine on loom threads: the main thread delivers batches and polls runs, worker threads poll runs alongside it, and each model checks its case's final values once everything settles.

- Models: Cases A, B, C, D, E, G, H, J, M, N, P, R, T, V, W and X, plus concurrent outside writes, a write during a run, disposal during a run, and a generator: the 20 models of v0.2.5, with Case J's rewritten for v0.3.0: an effect on a dropped source is freed, and a scoped one stops with its handle. Twelve more cover lifetimes: a last-handle drop racing a top-level creation on the same node; a parent's and its child's last handles dropping at once; a body anchoring a node for a moment while a sweep from upstream runs over it; one sweep's let-go racing another sweep's walk; a sweep inside an `update` closure whose drops raise further starting points; a sweep beside a fragile child's decision; a `move` body taking the only handle to a node whose watcher a sweep examines (Case AS); a scoped node's last handle dropping while a sweep from upstream runs over it (Case AT); an idle old child decided in the same step as its creator (Case AO); a supersede while the creator's run is being polled; a disposal while the body still creates (Case AR); and a write superseding an async block that created a node (Case AQ). v0.3.1 adds three for keyed nodes: a creator's rerun on a worker reclaiming a keyed child while the child's last outside handle drops on the main thread; a creator disposed while its rerun reclaims a keyed child; and a write superseding a creator's run after it created a keyed child, whose follow-up run reclaims that child. v0.4.0 adds nine: a creation racing the disposal of its only parent, and of one of two parents with a fragile child run on a worker; a handle a `move` block moved out dropping while the block's lease ends; a creator deciding its child before the child runs, unkeyed and keyed; a superseded async creator's creation waiting for the restart; a replacement run that new input does not replace before its item; a cut writer rerunning after its epoch; a `WHOLE_EPOCHS` join under concurrent writes; and a chain on the fast path on the worker while a second write frames it again. v0.4.0.2 adds two the TLA+ model led to: two runs writing one source in one epoch, and a run superseded after publishing an item that its successor computes equal. v0.5.0 adds one the lifetimes model led to: a handle moved out of a block dropped on another thread while the block's lease ends, after support let go of what it reached. That makes 48 models in `lithia-test` (`tests/loom.rs`). The lifetime models assert on each scope's live count, a per-node lifecycle counter loom does see.
- Determinism: sweeps keep their maps ordered by `NodeId`, and loom builds always take the full path for scoped nodes instead of a process-wide shortcut, so models running side by side in one process explore the same interleavings. Tokens that close together close in epoch-id order, not address order, for the same reason.
- The fast path: the models run with it, a run task polling the runs it keeps on its own loom thread and a step's runs packed for two workers, and again with `LITHIA_LOOM_FAST_PATH=off`, every ready run in a task of its own, the path it falls back to. CI runs both.
- Bound: a preemption bound of 3 by default: all 47 models explore about 490 000 interleavings with the fast path and about 810 000 without it, in one to two minutes on two test threads. At bound 4 the 45 models of v0.4.0.1 explore about 5.6 million with it and about 10.2 million without, and the two-writers model alone millions more; CI runs bound 4 both ways on every nightly push. `LOOM_MAX_PREEMPTIONS` overrides the bound.
- Teeth: each mutation below, applied to the implementation by hand before v0.4.0.1, made the named case, edge test or model fail. Later ones are kept as code (Mutations, below).

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
| Holding a scheduling lock while copying a parent list | the fragility model deadlocks |
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
| Not checking fragility again at a run start | the one-parent creation model |
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
| Forcing a rerun on every fragility recovery | Case BT |
| One lineage for all of a site's creations in a run | the sibling lineage edge test |
| Node ids instead of lineages in writer sets | Case S |
| No cut rerun | Case BU |
| A cut rerun that writes what it wrote | the suppression edge test |
| A cut rerun's writes to leave alone read when the cut is decided | the edge test of a cut rerun and writes recorded after the cut |
| `WHOLE_EPOCHS` not waiting | Case BV |
| Drop batches that never hand their drops on | the 20 000-node chain test |
| A freed child left in its parent's subscribers | the unread-memo edge tests |
| No waker for a run's first poll of a future | Case BI |
| A node's entries kept out of epoch order | the concurrent-writers edge test |
| A node written twice in one batch carrying its older stamp | the twice-written edge test |
| No delivery for a generator's later item when the poll wrote nothing else | the async source generator edge test |
| Writes to an async source node not stopping its block | Case AQ |
| A run start that keeps `Failed` | the fragility-recovery and panicking-comparator edge tests |
| The fast path keeping runs of another runtime | the runtime edge test of bodies on their own runtime |
| Runs left past a task's budget dropped instead of handed on | the wide fan-out and drop edge tests |
| A runner task dropped before its poll dropping its runs | the edge test of runs packed for a runtime that shuts down |
| Tokens of one epoch closed together counted as one | the 20 000-node chain test |
| A parent framed again before its release arrived counted once | the two-writers model |
| A superseded run's published item not carried to its successor | the published-item model |

  Dropping the walk's references under the sweep lock makes no model fail, since the operation context already defers the sweeps those drops raise. The rule stays so that no node's teardown runs under the lock. Leaving the holdover out of support's edges makes nothing fail either: a sweep then skips what lies past a held-over node, which can only delay a let-go, and releasing the holdover raises those nodes as starting points in the same operation. The edges stay so that a sweep sees the graph as it is. Starting a `changes()` stream on a ghost without checking makes no test fail either: the stream's first item finds the ghost and ends it, so the check only keeps a ghost from getting a channel. Packing the share a task keeps into a runner task as well makes none fail either: the run is then handed to two tasks, and still runs once, since a run takes its node's entries under the node's lock; only the spare task shows. Nor does a write's release reaching its node's subscribers at release time instead of those its framing walked: the two differ only for a node created between the two steps, which no model or test reaches.
- Not modelled: `Arc`/`Weak` reference counts and tokio's internals stay outside loom, so a race lying entirely between two loom-visible operations is collapsed. This is a limit of the method, not of the engine: every refcount-sensitive path in the protocol and in sweeps is bracketed by locks or atomics loom does see, and stress runs check the rest. Unlike v0.2.5's, the rewritten Case J model does not catch v0.2.5's Case J race, a run closing its tokens while still holding its node; stress runs are the check for that kind of race.

## Mutations

`.github/scripts/mutations.py` keeps each mutation as code, applies it to a copy of the workspace and
runs the test named for it, which must fail; `--list` names them. cargo-mutants also runs weekly over
`crates/lithia` in 16 shards, and lists the mutants the tests leave alive for review. W1–W4, F1–F5 and S1–S3 cover the
protocol, the fast path and support's walks; W3 and F5 are documented as surviving (W3 needs a node
created between a write's framing and its release, and F5 is equivalent to the original).

## Stress

`.github/scripts/stress.sh <release runs> <debug runs> <one-core runs>` runs every test binary that
many times, in release, in debug and pinned to one core (Linux only), and keeps the output of every
failed run. `extended.yml` runs it 200, 100 and 100 times.

## Miri

`extended.yml` runs the unit tests and validation cases, and the edge-case tests in three shards by
module, under Miri, with isolation and leak checks off for tokio's threads. Several tests stage races with sleeps, which Miri's
slowness can reorder, so the job fails only on undefined behaviour or an operation Miri cannot run.
