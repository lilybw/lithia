<!-- SPDX-FileCopyrightText: 2026 lithia Authors -->
<!-- SPDX-License-Identifier: MPL-2.0 -->

# Changelog

Every version's changes, newest first. Section numbers refer to that version's design spec in
[`docs/spec`](docs/spec); "Found by" names what found the need for a change. Nightly releases
(`v<version>.<n>`) carry the changes of their version as they land.

## 0.5.0 (unreleased)

v0.5.0 hardens lithia towards v1.0: a TLA+ model of node lifetimes found five faults, all fixed; the
open questions of v0.4.0 are decided; and the spec is trimmed to what a reader of the design needs,
with the validation cases, verification details, benchmark history and this changelog kept beside it.

| Section | Change | Found by |
| --- | --- | --- |
| 10.1 | A block's leased handles count as outside handles before its lease ends, so a leased handle dropped on another thread at that moment no longer takes an anchor off a count that does not include it. | TLA+ |
| 10.1 | An anchor count rises from zero only under the sweep lock, which walks downstream at once. A walk deferred to the end of an operation could run after the count had fallen again on another thread, and hold nodes no anchor reached; a sweep could miss a rise during it and let go of a node the new anchor reached. | TLA+; loom |
| 10.1 | A walk continues past the node it starts from, so a scoped node that is an anchor again holds again what depends on it. | TLA+; edge tests |
| 10.1 | A new writer registers under the sweep lock and, when supported, holds again what support let go of downstream of its targets; that node was freed once its other holder went, while the writer still wrote its input. | TLA+; edge tests |
| 10.1 | Walks run only while support has let go of a node that still lives, so a rise or a new writer costs a lock and no walk in the common case. | design |
| 8, 12 | A `WHOLE_EPOCHS` node waits for at most 64 epochs: once that many have become ready while it waits, it runs on them as a node without the flag would, and warns once per site. | review |
| 12 | The rerun-rate warning's default is 50 000 runs a second; a healthy node reruns 40 000–140 000 times a second. | benchmarks |
| 7.4 | Lineages stay 63-bit hashes rather than interned identities; the collision risk is documented. | review |
| 11.4 | A TLA+ model of node lifetimes beside the scheduling model, both checked with TLC in CI; TLC gates a release. A random differential test, 48 loom models, three more kept mutations. | review |
| 1 | lithia is licensed under the Mozilla Public License 2.0; up to v0.4.0 it was GPL-2.0-or-later. | review |
| 6.3 | A deferred function that panics is caught and logged, and returns its scope's token however it ends; `flush` waited forever after one. | review |
| 9.1 | A run that panics after it was superseded, or after a write stopped its block, no longer fails its node; a write's value stayed `Failed` until the next write. | review |
| 10.3 | Comparators, keys and kept inputs are dropped off the protocol, as values and bodies are, so their `Drop` cannot panic through a teardown or a refresh. | review |
| 12 | A panicking `tracing` subscriber is caught where lithia emits, so it cannot interrupt a protocol step; the rerun-rate warning is emitted after the node's rate lock is released. | review |
| 7.2 | An entry whose change a run in flight already read joins that run while it has not released, so `flush` and `defer` no longer return before the value that includes the change; once the run has released, the entry settles with the node's version. A TLA+ property and model (`AbsorbedReleased`, `MCAbsorb`) catch the old behaviour. | architectural review; TLA+ |
| 4.2 | Captures: a leading underscore no longer makes a constant a variable (`_LIMIT`); `use` items and statement macros such as `tokio::pin!` bind names; only a macro's format string is scanned, in every macro; `matches!`, `select!` and `tracing`'s fields are parsed; an unparsable macro takes every name in it for a possible write instead of capturing nothing; `{self}` in a format string is the captured clone, and a nested item or `use` keeps its own `self`. | review |
| 4.7 | Keys are compared after the creator's scheduling lock is released, so a key's `PartialEq` can no longer deadlock against the creator. | review |
| 11.4 | The fast path is closed while a body polls, so a body that blocks on `flush` no longer waits for runs only its own poll's end would start. | review |
| 3 | `Readable`, `Writable`, `Reflow` and `Dispose` require `Send + Sync`; `Failure` is `#[non_exhaustive]` and an `Error` whose source is its first cause; the panicking reads and writes carry `#[track_caller]`, and `try_get`, `changes`, `accessor` and `setter` are `#[must_use]`. | review |
| 6.3 | A deferred function fired where no runtime runs goes to one defer thread, started on first use, instead of a thread and runtime of its own. | review |
| 11.4 | Debug builds check a scheduling lock's invariants each time it is released; epoch tokens are a type that must be closed; the frame pointer is set only through a scoped call; failing and disposal change a node's phase in one step. | review |
| 11.4 | loom's model executor parks pending tasks until woken, so a lost wakeup fails a model; a clippy rule keeps locks, atomics and lazy cells loom cannot see out of the engine except where a use says why. | review |
| — | Ids, stamps and run ids are newtypes, writer ids are built only from a node or a lineage, and dense generic types are named by documented aliases; debug builds check token, anchor and live counts and version order with assertions. | review |
| — | The engine is organised by concern, in the spec's words: `graph` (ids, header, scheduler state, epochs), `protocol` (work, propagation, delivery, run lifecycle, linking, defers), `run`, `lifetime`, `exec`, `expand` (everything the macro calls) and `testing`. `Header` is split into identity, shape, lifetime, keyed and diagnostic parts; repeated protocol steps are named; a write and an item land through one commit, and freeing and disposal detach a node through one function; code outside the protocol uses named scheduler operations, and the trace `lithia::test::settled` follows lives in `testing`. `effect!`'s expansion is split by concern, the loom models by area; `lithia-test` holds the helpers the test crates share; `rust-toolchain.toml` names the toolchain, and the Miri shards are listed from the test files, so a new test module is never left out. | review |
| all | About 24 000 words instead of 41 000: the validation cases live in `crates/validation`, the verification details in `docs/VERIFICATION.md`, the benchmark history in `docs/BENCHMARKS.md` and the changes in this file. | review |

## 0.4.0

v0.4.0 empties section 14. Every behavioural inconsistency v0.3.1 tolerated is either fixed, so the same rule now holds in every context, or turns out to be a rule of a general-purpose core and is stated where it applies. lithia is the core of a family of engines (1): what only a restricted domain could make stricter belongs in a variant, not in an exception here. The changes were implemented and checked in stages, behaviour first, and this spec was written from the result. The last column says what settled each change: the design, or later the implementation and its checks.

| Section | Change | Found by |
| --- | --- | --- |
| 1 | Loud Failures lists what it cannot see, instead of pointing to section 14. lithia is the general-purpose core of a family of engines. | design |
| 3.2, 8 | A new flag, `WHOLE_EPOCHS`: a node starts no run while an epoch that has reached it is still on its way. | design; loom |
| 4.2, 4.5, 11.1 | Every body form sees its captures by reference. `move` moves instead of cloning in every form, and field paths refine it too. Bounds follow from what each form does with its captures. | design; implementation |
| 4.2, 5, 10.1 | A handle a `move` body takes over is leased to the expansion; an async source block ends the lease when it completes, is dropped, or its node is disposed, freed or refreshed, and the handles it moved out count from then on. | design; edge tests |
| 4.6, 6.4 | `ALWAYS_PROPAGATE` without `equals` is accepted and changes nothing, instead of a compile error or a panic. | design |
| 4.7, 10.1 | A reclaim needs the same shape: fragility, ghost policy and flags. A reclaimed node takes the creation's comparator; `init` applies only before a first run. | design; implementation |
| 7.1, 7.2, 10.1 | Creator order: a node its creator holds does not start a run while the creator may still replace or keep it. An old child no longer runs on the input that replaces it, a superseded run's creations wait for the restart, and a keyed child runs only with its creator's captures, once. A released child nothing can reach does not run. | design; loom |
| 7.2, 9.2 | Fragility recovery is a status change: a fragile child reruns only if a parent changed since its last run, and is otherwise restored. Fragility is checked again when a run starts, so a run decided before a parent failed does not publish over the failure. | design; loom |
| 7.3, 11.4 | A run that replaced a superseded run is protected until its first item, so an async node makes progress under fast input. | design; edge tests |
| 7.4 | A node a run creates writes under its lineage, and the writer check applies to first runs and refreshes, so loops through creation are cut. A cut writer reruns once its epoch settles, leaving alone what it wrote. | design; edge tests |
| 7.5, 8, 10.1, 11.4, 15 | Rules that were listed as exceptions: reads see writes before their epoch, first-run timing outside lithia tasks, self-support, classification by name, keyed identity, loom's blind spots, the sweep lock. | design |
| 10.1 | An anchor's return holds again what support let go of downstream. | design |
| 10.5 | A new child of a ghost behaves as a surviving child; with only ghosts for parents it is disposed at once. Disposal handles children linked while it runs. | design; loom |
| 11.3 | `lithia::test::settled` follows the subtree's writes. | design |
| 11.4 | 44 loom models, nine new; new mutation checks. | loom; mutation checks |
| 7.2, 11.4 | A child counts the releases each parent owes it, and each release reaches exactly the children its framing counted. Two runs writing one source in one epoch could leave a node below them waiting forever; a node framed again while its release was under way could settle its child on the release without the change. Fixed in v0.4.0.2. | TLA+; loom |
| 7.3 | An item published before its run was superseded keeps its stamp: the run that takes over the epochs releases with it, and a later item is delivered as usual. The children missed the change when the new run's item was equal. Fixed in v0.4.0.2. | TLA+; loom |
| 11.4, 16, 17 | A TLA+ model of the scheduling protocol, checked with TLC (`docs/tla`); 47 loom models; CI on macOS, ARM and the declared minimum rustc, with Miri, loom at bound 4 and stress runs on nightly. | review |
| 5, 7.4 | A cut writer's rerun reads what to leave alone when it starts. Read when the cut was decided, it missed writes the cut run's first poll recorded only after its own `spawn_blocking` write was cut, so Case AA could run its effect twice, about once in 200 runs under load. | stress runs |
| 11.2–11.4, 15–17 | The fast path: a run or runner task runs the ready synchronous runs of its runtime that its steps hand on, in a loop, up to 32 runs or 50 µs; a step that readies several packs them into about one runner task per worker; tokens of one epoch close together; hot statics on cache lines of their own. A run along a chain costs 1.25 µs instead of 3.9 µs, a 1 000-wide fan-out 1.2 ms instead of 4.0 ms. No API changes; a synchronous body may run in another run's task. 45 loom models, run with and without it. | review; benchmarks |
| 5, 10.3, 15 | A performance pass after a code review: values `equals` or a superseded run discards drop inline when they have no drop glue, as 10.3 already said, and strings and byte buffers join them; drops are gathered per step into one blocking task per runtime; a freed child leaves its parent's subscribers without a scan, so freeing many children of one node is linear; fewer allocations per run (17 to 10 on a chain). | review; benchmarks |
| 12 | Duplicate keys warn; effect cuts log at debug; the unread-memo warning is also checked when a memo loses its last reader. | design |
| 13 | Cases BL–BW; Case S rewritten; Cases T and BG extended. | validation |
| 14 | Empty. | |

Where v0.3.1's section 14 entries went:

| v0.3.1 entry | v0.4.0 |
| --- | --- |
| Loops through creation spin | cut by lineage (7.4, Case S) |
| A cut writer keeps a stale result | it reruns once its epoch settles (7.4, Cases T, BU) |
| Joins may mix epochs | `WHOLE_EPOCHS`; writes readable before their epoch is a rule (8, Case BV) |
| An async node can starve | a replacement run is protected until its first item (7.3, Case BS) |
| Fragility bypasses versions | recovery is a versioned status change (7.2, 9.2, Case BT) |
| A new child of a ghost panics | it behaves as a surviving child (10.5, Case BO) |
| Effects held in a replaced value | a rule: they live as long as the value (10.2) |
| `async move` block bounds | the bounds rule (4.2, 11.1) |
| `T` in synchronous bodies, `&T` elsewhere | every form sees `&T` (4.2, Case BL) |
| `move` means different things | one meaning; field paths refine it (4.2) |
| `ALWAYS_PROPAGATE` without `equals` | accepted, no effect (6.4, Case BM) |
| `settled` ignores writes | it follows them (11.3, Case BW) |
| loom does not see reference counts | a rule of the method (11.4) |
| Top-level nodes on long-lived state | a rule (10.1) |
| Self-support | a rule, with what catches it (1, 10.1) |
| Handles through containers count as writes | a rule of classification (4.2) |
| Handles moved out of an `async move` block go uncounted | leases (4.2, Case BP) |
| Letting go is final | an anchor's return holds again (10.1, Case BQ) |
| `Accessor` anchoring by kind | a rule (10.1) |
| The sweep lock and handle counts | a rule (15) |
| First-run timing outside lithia tasks | a rule (7.5) |
| An old child runs once more | creator order (7.2, Case BR) |
| The unread-memo warning at sweeps only | also when a memo loses a reader (12) |
| `.scoped()` is node-level | a rule (10.2) |
| Opaque captures count as changed | a rule (4.7) |
| A reclaimed node keeps its first options | shape and comparator (10.1, Case BN) |
| Keyed identity includes parents and write targets | a rule (10.1) |
| Duplicate keys at debug | a warning (12, Case BG) |
| What Loud Failures cannot see | listed in section 1 |

## 0.3.1

v0.3.1 adds keyed nodes. With `key` on `effect!`, a body's child keeps its identity across its creator's runs: a run reclaims the node its creator's previous run made with the same key, at the same site and with the same parents, instead of building a new one, and a reclaimed node whose captures are all equal does not run at all. A node created without `key` behaves exactly as in v0.3.0. The design was weighed against Solid 2.0's keyed `mapArray`, Leptos's `<For>`, Sycamore's `map_keyed` and Salsa's tracked structs, then implemented and verified before this spec was written. The last column says where each change was settled: in the design, or later by the implementation and its checks.

| Section | Change | Found by |
| --- | --- | --- |
| 1, 5.1 | A node created with a key keeps its identity across its creator's runs; every other node a run creates is new, as before. | design |
| 2 | New terms: key, keyed node, reclaim, holdover, inputs, probe. | design |
| 3.1, 3.2 | The `key` option: an expression, or a closure whose parameters name variables in scope; any `Hash + Eq + Send + Sync + 'static` type. | design |
| 4.6 | Errors for a key that is not `KeyValue`, given once on the key; an `async` or `move` key closure; a key parameter that is not a plain identifier. The unknown-option message lists `key`. | design; implementation |
| 4.7 | New: keyed creation. Captures are probed by reference before anything is cloned, through a ladder of handle, `PartialEq`, `Arc` pointer and opaque. The `Arc` level was added when Case BD's reordered rows, which capture a shared counter, reran. | design; validation |
| 5 | `Header` gains the value type, the key, the inputs and the block's run id; `Sched` gains the keyed set and the holdover. | implementation |
| 7.1, 8 | A refreshed derived node is seeded to rerun with its creation's writer set; a refreshed async source block restarts under the next run id. | implementation |
| 10.1 | Keyed nodes: the holdover, the conditions for reclaiming, same parents included, refreshing in place, write targets, duplicates. Children are rebuilt on every run unless keyed. | design; implementation; edge tests |
| 10.2, 10.4 | Nested effects can be keyed; disposal and the creator's freeing release the keyed set and holdover with the generation. | implementation |
| 11.4 | 35 loom models, three of them for keyed nodes; mutation checks for keyed nodes. | loom; mutation checks |
| 12 | Two debug events: a run that creates a key twice at one site, and a key outside any body. | implementation |
| 13 | Cases BD–BK. | validation |
| 14, 15, 16, 17 | Keyed residuals; the keyed-rows benchmark; node reuse leaves the roadmap and keyed collections join it; open questions on options and duplicates. | benchmarks |

## 0.3.0

v0.3.0 changes how long nodes live. A node with a body no longer needs a handle: a node created by a run is held by that run until its creator's next run is decided, and a node with parents created anywhere else is held by support, for as long as something outside lithia can still make it run. One opt-in, `.scoped()`, returns a node to v0.2.5's rule. The design was worked out in the v0.3.0 ownership migration (revision 12, in `docs/design`), reviewed in nine rounds, implemented on its own branch and verified there before this spec was written. Where the implementation found that the migration text was wrong or incomplete, this spec follows the implementation, and the last column says so.

| Section | Change | Found by |
| --- | --- | --- |
| 1, 10 | The lifetime rule: run generations, support with the graph as implicit root, and `.scoped()`. A plain source node and an async source block behave alike everywhere. | migration |
| 2 | New terms: generation, top-level node, support, anchor, outside and internal handle, scoped node, capture mode, read-only capture, field path, sweep, operation context, closed. | migration |
| 3.3, 4.6 | `#[must_use]` leaves `DerivedNode`; `.scoped()` on `SourceNode` and `DerivedNode` carries it instead. An `Accessor` to a plain source node anchors nothing. | migration |
| 3.5, 11.3 | `lithia::test::live_nodes` is async and drains the drops raised in the caller's scope before counting. | migration |
| 4.2 | Capture mode: the clones an expansion makes are internal handles, recorded with whether the body may write them. Read-only captures, and field paths that classify the handles of a value captured whole. Async source blocks see their captures by reference. `move` bodies are analysed by name, and the handles they take are flagged in place, so a moved binding keeps the user's `mut`. Arguments of macros other than formatting macros, `vec!` and `dbg!` count as possible writes. | migration; implementation |
| 4.2 | An async body that ends in a `loop` it never leaves has its output typed `()`, so `effect!(async \|cfg\| loop { … })` compiles under the 2024 edition's never-type fallback; `-> T` on an async closure types its output. | implementation |
| 4.5 | Expansion sketch: the capture session, and the recorded captures passed to creation. | migration |
| 4.6 | A value that is neither `Send` nor `Sync` gets one error instead of two; a capture that is not `Clone` is labelled where it was captured and carries its fix. | release check |
| 5 | `Header` gains the anchoring count, the captured count, the support self-reference, the scoped flag, the generation's back-link and the writer and target lists; `Sched` gains the generation, `closed` and `releasing`; handles gain the internal flag. | migration; implementation |
| 6.2, 10.1 | Blocking work started by a run creates into that run's generation, or under no creator once the run's node is gone. | implementation |
| 7.2, 7.3 | Releasing generations: when a node's next run is decided with no run in flight, at every run start, when a superseded run or an async block superseded by a write has stopped, at disposal and when the creator is freed. A node releasing a stopped run's generation is not quiet until the release is done. | migration; loom |
| 10.1 | Sweeps: one sweep lock, an operation context per thread, an exact and bounded check of each starting point before the walk, the region walk with trusted support-held nodes at its border, and the least fixpoint. Cycles and self-support. | migration; implementation |
| 10.2 | Rewritten: no handle needed; `.scoped()` or `dispose` to stop early; the `effect!(async \|cfg\| …)` idiom; binding fields before `effect!`. | migration |
| 10.3 | A freed node wakes its parked run, which then drops its future and the captures in it. | validation case AN |
| 10.4, 10.5 | Disposal closes the node, releases its generation and drops its support; a ghost is never an anchor. | migration |
| 11.3 | Creating a derived node or an async source block still needs a tokio runtime context, so "a bare thread" in the migration means a thread that has entered one. | edge tests |
| 11.4 | Step 1: the sweep lock in the lock order, the parent-list copy, references dropped after unlocking. Step 8: 32 loom models, including 12 for support; models stay deterministic side by side. Mutation checks. | migration; loom |
| 12 | One first-run warning replaces the freed-before-first-run warning and its value-drop exemption; growth and unread-memo warnings; a trace event per sweep. | migration; implementation |
| 13 | Case J rewritten, Case AH replaced, Cases N, U and W check `live_nodes`, Cases AI–BC added. | migration |
| 14, 15, 16, 17 | The migration's residuals; benchmarks of handles, creation, sweeps and drop latency; its roadmap items; the four questions it settled. | migration; benchmarks |

## 0.2.5

v0.2.5 aligns the spec with the v0 implementation: no semantics were reversed, but the protocol gained rules that v0.2.4 left implicit, and several details were fixed by what the code and its tests showed.

| Section | Change | Found by |
| --- | --- | --- |
| 3, 13 | Validation cases live in the `lithia-validation` crate, not a `lithia::validation` module; edge-case tests in `lithia-test`. | crate layout |
| 3.2 | Option constants resolve inside `effect!` without an import. | macro design |
| 3.3 | The four handle traits are sealed; every handle is `Debug`; `Changes` has an inherent `next`; `Ghosted` implements `Error`. | implementation |
| 3.4, 4.3 | `Site` implements `Display` and a hand-written `Debug`; `Failure` displays as `message (site)`. | implementation |
| 3.5 | `#[lithia::main]` and `#[lithia::test]` accept `flavor`, `worker_threads` and `start_paused`; `LITHIA_TEST_TIMEOUT_MS` bounds each test. | Case N, test harness |
| 4.2 | Capture rules made precise: method-syntax capture through one reference, format width and precision names, named format arguments, nested `effect!` parsed, unused dependencies not captured. A non-`Clone` capture is a compile error suggesting `Arc`. Async and generator bodies see captures by reference from one shared `Arc`; `move` there moves rather than clones. | compile-fail tests |
| 4.5 | Expansion sketch updated: `__lithia_scope` marker, `CaptureAny`, `InitDefault`, the options builder, generator lowering. | implementation |
| 4.6 | Messages for a non-`Clone` capture, a `Setter` dependency, a non-`async` entry fn and an unknown attribute argument; `ALWAYS_PROPAGATE` checked at runtime when `flags` is computed. Every message has a compile-fail test. | compile-fail tests |
| 5 | Node, scheduler, entry and run records rewritten to match the implementation (carry stamps, seeds, handovers, flags per entry). | implementation |
| 6.3 | A pending `defer` holds a token on its scope, so `flush` waits for it; outside a lithia task it attaches to the scope's latest epoch. | Case X |
| 6.4, 8 | An equal write under `ALWAYS_PROPAGATE` counts as a run and supersedes an async block. | Case AD |
| 7.1 | New nodes stay unseeded until their creation is delivered; entries opened at link time wait for the seed. | Case N |
| 7.2 | One obligation per epoch: a node owes its children at most one release per epoch; framing stops at nodes already framed; releases with no matching bit are ignored; a reflow-only entry waits for the run in flight instead of superseding. | Case T, stress tests |
| 7.3 | Supersession is a cancel flag plus a wake, taking effect at the poll boundary; the last poll's batch is still delivered; served epochs pass to the next run. A run's release defers into an open entry of the same epoch. | Case T, loom |
| 7.4 | Seed writer set = creator's writer set minus the creator; disposal survivors rerun unattributed; cut logs name the written node at the head of the chain. | Cases U, AC |
| 8, 3.6 | A source node's reflow sends nothing to `changes()`. | Case V |
| 10.1, 11.4 | Run tasks drop every strong reference to their node before closing epoch tokens, so a settled scope never sees a node kept alive by lithia. | stress-testing Case J |
| 10.1, 10.3, 12 | Bodies release captured handles off-thread; values without drop glue drop inline; the freed-before-first-run warning is skipped while lithia drops a value; panicking drops are logged. | implementation |
| 10.4 | Disposal walks descendants in `NodeId` order and treats ghost parents as not live. | Case AC |
| 11.3 | Detached scopes are keyed by tokio task id or thread; `settled` covers nodes created by the subtree's runs and checks parents first against a progress counter. | Case U, loom |
| 11.4 | Step 1: diagnostics, like spawns and wakes, run after the lock is released. Step 4: the fast path is left out of v0. Step 8: loom model checking done (20 models, bound 3); TLA+ pending. | loom, code review |
| 12 | Rerun threshold set by `LITHIA_RERUN_RATE` (default 1 000/s). | implementation |
| 14, 15, 16, 17 | Five new tolerated inconsistencies; first benchmark results; fast path and TLA+ on the roadmap; two open questions resolved, two added. | all of the above |

## 0.2.4

The first design spec, implemented as lithia v0.
