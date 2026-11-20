# lithia implementation progress

Source of truth for resuming work. Spec: `docs/spec/LITHIA_design_spec_v0.4.0.md` (PDF alongside), which empties
v0.3.1's section 14; v0.3.1, v0.3.0, v0.2.5 and v0.2.4 are kept for reference.

## Layout

| Crate (dir) | Package | Role |
|---|---|---|
| `crates/lithia` | `lithia` | Engine and public API: nodes, handles, epochs, scheduling protocol, drops, diagnostics; re-exports the macros; `lithia::test` utilities |
| `crates/macro` | `lithia-macro` | `effect!`, `#[lithia::main]`, `#[lithia::test]` |
| `crates/validation` | `lithia-validation` | Spec §13 cases, verbatim (`tests/cases.rs` A–AG, `tests/support.rs` AI–BC, `tests/keyed.rs` BD–BK, `tests/consistency.rs` BL–BW, `tests/compile.rs` AH) |
| `crates/test` | `lithia-test` | Edge-case tests: `tests/edge/` (one module per area, one binary), `tests/deep_chain.rs`, `tests/loom.rs` |
| `crates/bench` | `lithia-bench` | Spec §15 benchmarks |

## Validation cases (spec §13)

All of A–BW pass (75 cases). Stress for v0.3.0: every test binary (cases, both `support` files, and the
seven other `lithia-test` files) 400/400 release runs, 200/200 debug runs, 250/250 single-core runs.
Stress for v0.3.1: all 12 binaries, both `keyed` files included, 100/100 release, 50/50 debug and 60/60
single-core runs; 155 workspace tests; 35 loom models at bounds 3 and 4.
v0.4.0: 44 loom models at bound 3 (~620 000 interleavings, ~72 s) and bound 4 (~9.5 million, ~17 min); the
new cases 25/25 runs and 10/10 single-core, the edge suite 15/15 and 5/5 single-core. After the performance
pass: 197 workspace tests; every validation and edge binary 10/10 runs and 3–5 single-core. After the
fast path: 203 workspace tests; every binary 10/10 and 3/3 single-core, the edge suite 20/20, 5/5
single-core and 15/15 release; the cases binary 600/600 in three parallel loops.

| Case | Status | Case | Status |
|---|---|---|---|
| A nested creation | pass | R equal parent | pass |
| B diamond | pass | S loop through creation | pass |
| C clamp cycle | pass | T refused writer | pass |
| D optimistic async | pass | U loader feeds creator | pass |
| E panic + fragility | pass | V reflow | pass |
| F ghosts | pass | W reflow from own body | pass |
| G async source | pass | X defer | pass |
| H cycle sibling | pass | Y changes() | pass |
| I returned memo | pass | Z modify | pass |
| J effect lives while its parent can change | pass | AA spawn_blocking | pass |
| K generator | pass | AB SEMI_FRAGILE | pass |
| L yield before await | pass | AC ghost survivor | pass |
| M clamp via memo | pass | AD ALWAYS_PROPAGATE | pass |
| N mid-propagation creation | pass | AE panic propagates writes | pass |
| O yield nothing | pass | AF live_nodes | pass |
| P unconditional write-back | pass | AG two runtimes | pass |
| Q cut scoped to item | pass | AH bare effect / scoped must_use (trybuild) | pass |

Cases AI–BC (v0.3.0, `tests/support.rs`): AI motivating example, AJ writer keeps watcher, AK reader keeps
nothing, AL children rebuilt, AM stored child outlives rerun, AN async blocks (discarded, aborted, bound
feeder, loop on `cfg`), AO idle old child, AP captures internal, AQ supersede releases creations, AR
creations under a disposed body, AS `move` bindings, AT per-request memos and `.scoped()`, AU effect on
nothing held, AV support through a sent handle, AW self-support, AX child writing creator's parent, AY field
paths, AZ escaping `Accessor`, BA scoped child in a value, BB scoped field, BC both kinds of source alike: all pass.

Cases BD–BK (v0.3.1, `tests/keyed.rs`): BD rows keep their nodes, BE keyed source nodes take new values, BF new
parents make a new node, BG duplicate keys, BH keyed async nodes, BI a superseded creator hands its keyed nodes on,
BJ a key outside a body, BK captures without `PartialEq`: all pass.

Cases BL–BW (v0.4.0, `tests/consistency.rs`): BL captures by reference, BM `ALWAYS_PROPAGATE` without
`equals`, BN reclaim shape and comparator, BO new children of ghosts, BP a handle moved out of a block, BQ an
anchor returns, BR creator order, BS progress under fast input, BT fragility recovers by status, BU a cut
writer reruns, BV `WHOLE_EPOCHS`, BW `settled` follows writes: all pass. Case S is rewritten (a loop through
creation is cut); Cases T and BG are extended.

## Spec sections

| Section | Status | Notes |
|---|---|---|
| §3 Public API | done | all handles, views, traits, free functions, `lithia::test` |
| §4.1–4.5 effect! expansion | done | deps, capture, site, init, generator lowering |
| §4.6 compile errors | done | every message covered by `tests/ui` (trybuild) |
| §4.7 keyed creation | done | v0.3.1: probes, lookup, `NodeKey` (`crates/lithia/src/keyed.rs`, `crates/macro/src/effect.rs`) |
| §5 Node model | done | |
| §6 Writes, batching, defer, equals | done | |
| §7 Propagation | done | see protocol notes below |
| §8 Pending/async/generators | done | |
| §9 Failure, fragility | done | |
| §10 Lifetime, disposal, ghosts | done | v0.3.0: generations, support, sweeps, `.scoped()` (`crates/lithia/src/support.rs`, `capture.rs`); v0.3.1: keyed nodes, holdover (`keyed.rs`, `create.rs`) |
| §11 Concurrency, scopes | done | fast path and runner tasks (§11.4 step 4, `crates/lithia/src/inline.rs`) since v0.4.0 |
| §11.4 step 8 loom | done | 47 models, bound 3 by default, bound 4 on nightly; see "Model checking" below |
| §11.4 step 8 TLA+ | scheduling done | `docs/tla`: framing, releases, runs, supersession, cuts, epoch tree; creation and lifetimes still to model (§16); gating is an open question (§17) |
| §12 Diagnostics | done | rerun threshold via `LITHIA_RERUN_RATE` (default 1000/s); first-run, growth and unread-memo warnings |
| §15 Benchmarks | done | results and observations in `docs/BENCHMARKS.md` |

## Decisions and interpretations

- Validation cases live in the `lithia-validation` crate rather than a `lithia::validation` module, per the six-module layout.
- Test harness timeout: `.cargo/config.toml` sets `LITHIA_TEST_TIMEOUT_MS`, which `#[lithia::test]` honours.
- A node owes its children at most one release per epoch (an open entry, or a run holding the epoch unreleased); a run's release defers into an open entry of the same epoch. This keeps joins glitch-free when a writer is re-framed mid-run (Case T). Since v0.4.0.2 a child counts the releases each parent owes it (a source written twice in an epoch, or a parent re-framed while its release is under way, owes two), and each release reaches exactly the children its framing counted: a framing walk goes on through the subscribers a node had when its entry opened, and a release reaches the subscribers its node had when it was gathered, or for a write, those its framing walked.
- New nodes do not run until their creation is delivered (seeded); entries opened at link time wait for the seed (Case N runs `m` once).
- `settled(&node)` waits on the node, its descendants and nodes created by their runs (needed for Case U).
- `defer` holds a token on the caller's scope until its task delivers, so `flush()` also waits for deferred work.
- A write equal under `ALWAYS_PROPAGATE` counts as a run (supersedes an async block feeding the source).
- Non-`Clone` captures are a compile error with a hint to wrap in `Arc` (spec §4.2 "wrongly captured … fails to compile").
- `move` async closures and generators move their captures into the shared `Arc<Captures>` instead of cloning.
- Run tasks drop every strong reference to their node before closing epoch tokens, so a settled scope never sees a node kept alive by lithia (found by stress-testing Case J).

- Reflow of a source node re-emits its value to its children but sends nothing to `changes()`, since the value did not change.
- Diagnostics raised while a scheduling lock is held are gathered and emitted after it is released (spec §11.4 step 1).
- `lithia-test` holds 109 edge-case tests: 108 in `tests/edge/` (values, propagation, async/generators, lifetime,
  capture, diagnostics, runtimes, support, keyed nodes; one module each, sharing one tracing collector in
  `events.rs`) and the 20 000-node chain in `tests/deep_chain.rs`. The chain runs alone because a sweep
  through handleless nodes walks to the top of the chain while any scoped node is alive, which makes
  building it beside other tests' scoped nodes quadratic (see Next steps).

- v0.4.0 (spec §14 emptied; `docs/design/v0.4.0-consistency.md` has the plan and what the implementation
  settled): captures by reference in every form; leases for handles a `move` block takes over; reclaim by
  shape, comparator adopted; unread memos checked when a reader goes; `settled` follows writes; children of
  ghosts; re-holding on anchor return; creator order at run start; protected replacement runs; fragility
  recovery by status; lineages in writer sets; cut writers rerun; `WHOLE_EPOCHS`.

## Model checking (loom)

- `--cfg lithia_loom` (not `--cfg loom`, which tokio reacts to) swaps the engine's mutexes, atomics,
  thread-locals, value cells and version clock for loom's through `crates/lithia/src/sync.rs`, and runs
  bodies on a model executor (`crates/lithia/src/model.rs`) instead of tokio.
- Models live in `crates/test/tests/loom.rs`: Cases A, B, C, D, E, G, H, J, M, N, P, R, T, V, W, X,
  plus concurrent outside writes, a write during a run, disposal during a run, and a generator; and since
  v0.3.0 twelve lifetime models (spec §11.4), which assert on `ModelScope::live()`; v0.3.1 adds three for keyed
  nodes (a reclaim racing a last-handle drop, a disposal during a reclaim, a supersede handing keyed nodes on);
  v0.4.0 adds nine (spec §11.4), two of which found races the release fixes, and one for a chain on the
  fast path racing a second write; v0.4.0.2 two the TLA+ model led to (two runs writing one source in one
  epoch; a run superseded after publishing).
- Default preemption bound 3: all 47 explore ~490 000 interleavings with the fast path, ~810 000 with
  `LITHIA_LOOM_FAST_PATH=off`, in one to two minutes on two test threads. Bound 4 (v0.4.0.1's 45): ~5.6
  million with it, ~10.2 million without; the two-writers model alone explores millions more at bound 4.
  `extended.yml` runs bound 4 both ways on nightly.
- A model must not hold a std lock across anything loom can switch on: a body that locks a std `Mutex` and
  then reads a node blocks the scheduler's only running thread. Read first, then lock.
- Sweeps use ordered maps and loom builds always take the scoped path, so models stay deterministic when
  several run in one process.
- Mutation checks for v0.3.1 keyed nodes: 10 mutations; nine are caught (spec §11.4 table), and leaving the
  holdover out of support's edges is not, as the spec explains (it can only delay a let-go).
- Mutation checks for v0.3.0 (spec §11.4 table): 19 mutations, each applied as a patch and reverted; each
  fails a case, edge test or model. Dropping the walk's references under the sweep lock fails nothing (the
  operation context defers the sweeps it raises), and the rewritten Case J model does not catch v0.2.5's
  Case J race; both are noted in the spec.
- Teeth (v0.2.5): reintroducing the Case J race made the then Case J model fail at bound 3
  (`(2, 2)` vs `(2, 1)`); the v0.3.0 model checks live counts and a scoped effect instead, and does not.
- Not modelled: `Arc`/`Weak` reference counts and tokio internals stay outside loom. Races that sit
  entirely between two loom-visible operations are collapsed; every refcount-sensitive path in the
  protocol is bracketed by locks or atomics loom does see, which is how the Case J mutation is caught.
- Models never wait on async results with `flush`; like tests, they use `ModelScope::settle(&node)`.

## Working notes

- Stress loop used before a release check: run each test binary 100–400 times in debug and release, and
  again pinned to a single core with `taskset -c 0`.

## Spec v0.2.5

- Written after the v0 implementation, from the code itself: every section re-checked against the source,
  the compile-fail messages and the tests. Its "Changes since v0.2.4" table lists each update and what found it.
- Kept as a Claude doc (editable, commentable) and exported to `docs/spec/` as Markdown and PDF; the
  write-routing diagram of §7.4 is `docs/spec/write-routing.png`.
- Code conventions applied in the same pass: every named binding and closure parameter is typed, and doc
  comments are at most two lines, with longer rationale moved into the spec.

## Spec v0.3.0

- Written after implementing and verifying the ownership migration on branch `v0.3.0-ownership`, merged
  into `main`. Its "Changes since v0.2.5" table says for each change whether the migration or the
  implementation found it; the migration doc's section 17 lists where the implementation corrected it.
- Exported to `docs/spec/` as Markdown and PDF (pandoc + Chromium); the v0.2.5 Claude doc is not updated.

## Spec v0.3.1 (keyed nodes)

- `key = expr` or `key = |a, b| expr` on `effect!` inside a body: the creator's next run reclaims the node with
  the same key, site, kind, value type and parents instead of creating one, and reruns it only if a capture
  changed. Keyed nodes of a released generation wait in the creator's holdover until a run ends unsuperseded.
- Captures are probed by reference before any clone (handle by node, `PartialEq` by value, `Arc` by pointer,
  else opaque); a changed derived node is refreshed in place and seeded to rerun, an async block restarts,
  a plain source node is written. Different write targets give a new node. Options stay as first created.
- Implemented on branch `v0.3.1-keyed`, then written into the spec, which says what the implementation settled.
- Parked: option C, a keyed collection node (spec §16).

## Spec v0.4.0 (section 14 emptied)

- Every v0.3.1 section 14 entry was checked, still held, and is now fixed or a rule where it applies:
  async runs make progress; captures are references in every form; all three cycle and epoch entries are
  fixed; by-design entries became rules (Lily's decisions, 2026-11-18). lithia is the general-purpose core
  of a family (LiSP, lithia-gui; project doc `ECOSYSTEM.md`), so restrictions belong in variants.
- Implemented in stages on branch `v0.4.0` (macro and diagnostics; lifetimes; protocol), each tested,
  model-checked and mutation-checked before the next, then written into the spec, whose changes table says
  where each entry went and what settled it. Loom found two races on the way, both fixed.

## Performance pass (2026-11-20, branch `v0.4.0`)

- A reviewer's list of non-breaking performance items (Claude doc "lithia: non-breaking performance
  improvements", written against v0.3.1) was checked against v0.4.0, measured, and the substantiated
  items implemented: discarded values follow the `needs_drop` rule (a bug), strings and byte buffers drop
  inline, drops are gathered per step into one blocking task per runtime, fewer allocations and locks per
  run. Profiling found one item the review missed: freeing many children of one node was quadratic
  (`Header::unsubscribe` scanned a `Vec`); subscribers are now a map by id.
- Results in `docs/BENCHMARKS.md` and spec §15: drop churn, rebuilding and keyed rebuilds 68–80% faster,
  runs 19–32% cheaper, allocations per chain run 17 to 10. New benchmarks: discarded values, wide
  creation, and `benches/allocs.rs`.
- Checked by four new edge tests (drops once per payload, discarded values, late streams, a node written
  twice in one batch), a unit test of the inline types, the loom models at bounds 3 and 4, and mutation
  checks (spec §11.4 table).
- The review's other items are recorded as done or left out, with reasons, in `docs/BENCHMARKS.md`.

## Fast path, runner tasks and many-core work (2026-11-20, branch `v0.4.0`)

- Decided with you: the fast path (review 1.1) and runner tasks (1.2) go in, being non-breaking (no API
  change; a synchronous body may run in another run's task); merging lock round trips (1.5) is out of
  v0.4.0; pooled scratch buffers and `RunShared` reuse (2.5, 2.6) move to v0.4.1; inline writer sets
  (2.3), striped support counters (3.3), a slimmer node (3.4), sweep buffers (5.4) and a coarse rerun
  clock (5.5) are dropped as not significant. lithia targets 8–64 cores, so many-core items count even
  where this 2-core container cannot measure them.
- `inline.rs`: a run or runner task keeps the ready synchronous runs of its runtime its steps hand on
  (up to 16 queued) and polls each as its own `RunFuture` in a loop, up to 32 runs or 50 µs; cheap
  leftovers go on in one runner task, heavy ones get tasks of their own. A step readying several
  synchronous runs packs them into runner tasks of `ceil(n / workers)` clamped to 1..=16, the task keeping
  the first share when its fast path is open for that runtime. Fast-path runs close their tokens at the
  end of the task's poll, coalesced per epoch. Loom builds take it unless `LITHIA_LOOM_FAST_PATH=off`;
  CI runs the models both ways.
- Many cores (review 2.8, 5.2): hot statics are 128-byte aligned (`sync::Padded`), since `nm` showed the
  clock and id counters on one line and the settle signal, sweep lock and drop count on another. Batched
  token opens were not done: deferring an open past the framing walk lets a concurrent disposal close
  that entry's token first and settle the epoch early. Closes, which do contend, are coalesced instead.
- Results (`docs/BENCHMARKS.md`, spec §15): a run along a chain 3.9 → 1.25 µs, a 1 000-wide fan-out
  4.0 → 1.2 ms, keyed rebuilds a further 16–34%, wide creation 67 → 42 ms.
- Found by stress runs while checking it: Case AA could run its effect twice (about 1 in 200 runs under
  load, in v0.4.0 as first committed too). A cut writer's rerun read what to leave alone when the cut was
  decided, which can precede the cut run's own first-poll writes being recorded; it now reads it when
  the rerun starts (`Sched::suppress_in`, spec §7.4). A deterministic edge test covers it.
- Checked by six edge tests (wide fan-out, long chain, a panic on the fast path, bodies on their own
  runtime, runner tasks across a shutdown, the cut-rerun race), one more loom model, and mutation checks
  (spec §11.4 table).

## Verification follow-up (2026-11-20, branch `verify/v0.4.0.2`)

After an adoption-readiness review of def9487 (v0.4.0.1):

- Stress at v0.3.0's level on v0.4.0.1: every test binary (cases, consistency, deep_chain, edge, keyed,
  support, the unit tests) 400/400 in release, 200/200 in debug and 250/250 pinned to one core; no
  failures. `.github/scripts/stress.sh` runs the same on Linux x86-64 and ARM and on macOS in CI.
- TLA+ (§11.4 step 8): `docs/tla/Scheduling.tla` models the scheduling protocol at the grain of its lock
  sections; six TLC models check token conservation, waits only on owed releases, consistency whenever
  `flush` returns, glitch freedom and termination. TLC found two races in v0.4.0.1, and the loom model
  written for the first exposed a third (`docs/tla/README.md`, spec changes table):
  two runs writing one source in one epoch could leave a node below waiting forever; a parent framed
  again while its release was under way could settle its child without the change; a run superseded
  after publishing dropped its item's stamp. Fixed by counted waits, releases that reach exactly the
  children their framing counted, and carrying the stamp to the successor. An intermediate fix, an
  entry per written node, was dropped once counting alone sufficed: it cost about 115 ns per outside
  write.
- TLC on the fixed protocol, two workers, exhaustive and with no error: `MCSupersede` 201 709 states
  (depth 77, 2 min), `MCAsyncChain` 1 085 317 (depth 101, 9 min), `MCWriteBack` 2 645, `MCBinding` 1 145
  (seconds each). `MCSharedTarget` and `MCDiamond` passed several million states without error; their
  complete runs take hours here and run in CI.
  Mutating the model the way the fixes changed the code makes it fail again: waits that do not count
  break `NoStuckWait` on `MCSharedTarget` (24 s), and dropping the successor's stamp breaks
  `ScopeConsistent` on `MCSupersede` (52 s).
- Loom: 47 models, both new ones fail on v0.4.0.1 and pass now, at bound 3 both ways; bound 4 runs on
  nightly.
- CI: macOS and ARM join the gating test matrix (loom stays on Linux and Windows); a job runs the tests
  on rustc 1.95, the declared minimum, and passes. `extended.yml`, on nightly pushes, weekly and pull
  requests: Miri, loom at bound 4 both ways, stress runs, TLC per model, the kept mutations. Miri found
  no undefined behaviour in the unit tests and the 45 validation cases before Case BV, which fails under
  Miri only because Miri's slowness reorders the race its sleeps stage; the job now lists such failures
  and fails only on undefined behaviour. Branches named `verify/…` run both workflows on push.
- Mutations: `.github/scripts/mutations.py` keeps eight as code (W1–W3, F1–F5) and applies each to a copy
  of the workspace; W3 and F5 are documented as uncaught. The older rows of the spec's table were applied
  by hand and are not kept as code; no cargo-mutants run yet.
- Benchmarks, A/B against v0.4.0.1 over seven alternating runs: no row moves beyond run-to-run noise
  (about ±10% on this container); allocations per run unchanged, one fewer per write and per creation.
- Docs: the README's glitch-freedom claim is limited to one batch; CONTRIBUTING describes the new CI and
  moves `release` to a nightly release that has passed the extended checks and soaked, not to
  `nightly`'s tip.
- Not done here: benchmarks and stress on 8–64 cores, which this 2-core container cannot give.

## Design migration to v0.3.0 (ownership)

- `docs/design/v0.3.0-ownership-migration.md` (also a Claude doc), revision 12 plus its implementation notes. A node created in a run is
  held by that run's generation until its creator's next run is decided; any other node with a body is held
  by support: while an anchor (a node with an outside handle) reaches it along parent, creation and writer
  edges. Sweeps let go of nodes support no longer reaches. No identity, no declarations, no process-wide owner.
- Eight independent review rounds (protocol, API, simplification); all three judged revision 9 ready to
  implement, and revision 10 makes their last wording fixes.
- Revision 12 answered the open questions (`.scoped()`, the unread-memo warning, async source blocks living
  by their handles, captures by reference). Implemented, verified against section 13 and merged into spec
  v0.3.0; section 17 records the corrections the implementation made.

## Release check (2026-11-12, branch `release/v0.3.0`)

Five passes from `TODO.md`, run in series:

- Dependencies: `lithia-macro` on syn 3 (match guards now live in the arm's pattern); parking_lot 0.13,
  which raises the MSRV to 1.95; tracing without default features, which drops the last syn 2. No duplicate
  crates remain; every other dependency was already at its latest release. Toolchain checked: 1.97 only.
- Simplification: `lithia-core` merged into `lithia` (five crates); one frame `Target` replaces three
  enums; redundant clones gone. The scheduler itself was left as it is, by decision.
- Modernization: `LazyLock` and const-initialised mutexes for statics, `ptr::from_ref(..).addr()` for site
  keys, `#[diagnostic::do_not_recommend]` on the `NodeValue` and `Capture` blanket impls (spec 4.6).
  `std::sync::nonpoison` is still unstable, so parking_lot stays. Solid 2.0 (beta/RC) makes writes visible
  to reads only after `flush()`; lithia keeps immediate visibility (spec 6), a deliberate difference.
- Spec sync: every 4.6 message, the 3.3/3.5 API and capability table, options, the 4.2 macro and method
  lists, the section 12 table, all 55 cases and the stated test counts checked against the code; spec 4.6,
  its date and change table, the PDF and the README updated.
- Validation: 136 workspace tests, 32 loom models at bound 3 (31 s) and bound 4 (7.7 min); clippy (normal
  and loom cfg), rustfmt and `cargo doc` clean with `-D warnings`; every test binary 100/100 release,
  50/50 debug and 60/60 single-core runs; 21 mutations re-run: the 19 of spec 11.4's table are caught, and
  the two the spec names as uncaught still are. Benchmarks A/B against `main`: no regression.
- Not done: Miri and the 1.95 toolchain need downloads this container cannot make; the TLA+ model the
  spec requires before shipping (11.4 step 8) is still open.

## CI and nightly releases (2026-11-18)

- `.github/workflows/ci.yml`: on Ubuntu and Windows with rustc 1.97.0 (the compile-fail snapshots depend on
  it), as four parallel jobs (tests and loom per OS): fmt (Ubuntu), clippy normal and loom cfg,
  `cargo doc -D warnings`, a docs-site snapshot (Ubuntu), `cargo test --workspace`, the loom models at bound 3.
  Line-table debug info, and rust-lld on Windows: compiling and linking were most of the Windows job.
  Also runs on pull requests into `nightly` or `release`; manual runs need the workflow on `main`.
- `.github/workflows/nightly.yml`: every push to `nightly` runs `ci.yml`; when both platforms pass,
  `.github/scripts/release-assets.sh` names the release `v<crate version>.<n>` (one above the highest `n`
  for that version; a commit already released is skipped) and `gh release create` attaches the two crate
  packages, the API docs, the newest spec PDF and `SHA256SUMS`. Runs are serialized, so numbers stay in
  push order; a push that arrives while one waits replaces the waiting one.
- Packaging flips `publish = false` to `true` on the runner only: cargo verifies `lithia` against the
  packaged `lithia-macro` only for publishable packages. The crates have no license yet, so cargo warns.
- Branches: `nightly` (released when green) and `release` (stable merges; v0.3.1 merged 2026-11-18). The old
  `release/v0.3.0` branch gave way to `release`; its tip 003d740 is in `release`'s history (no tag: this
  session cannot push tags).
- Docs site (GitHub Pages, deployed by Actions, live at https://lilybw.com/lithia/): `pages.yml` runs after
  each release; the `github-pages` environment allows `nightly`. Each
  release's `lithia-docs-<tag>.tar.gz` holds `api/` (rustdoc with `docs/site/rustdoc.css`: Lora as the text
  font, self-hosted under OFL; plus the forwarding script) and `spec/` (the newest spec via pandoc and
  `docs/site/spec-template.html`). `.github/scripts/site.py assemble` lays the newest 50 out under
  `versions/<v>/`, the newest also at `api/` and `spec/`, with a front page, `versions.json` and a 404 page.
  `?v=<version>` on a latest page forwards to `versions/<version>/…` (`docs/site/ts/version.ts`, compiled by
  `tsc` 5.9.3 at build time). v0.3.1.1 and v0.3.1.2 predate this: rustdoc only, the spec as PDF.

## Public release preparation (2026-11-18)

- License: GPL-2.0-or-later, Blender's style: `LICENSE` holds the GPL v2 text, every source file starts with
  `SPDX-FileCopyrightText: 2026 lithia Authors` and `SPDX-License-Identifier: GPL-2.0-or-later`, and
  `AUTHORS` lists the lithia Authors. The Lora fonts in `docs/site/fonts` stay under the OFL 1.1. Cargo
  metadata names the license, the docs site and the API reference.
- README summarizes lithia for users; `CONTRIBUTING.md` holds the layout, commands, conventions and the
  pull-request flow into `nightly`; `.github/CODEOWNERS` and a PR template support it.
- Branch protection is a repository setting this session cannot write: a ruleset on `nightly` and
  `release` (no deletion or force push; pull requests with one code-owner approval; the four CI checks;
  repository admins may bypass) is set up by hand.
- Docs site styling: `docs/site/site.css` (front pages and spec) and `docs/site/rustdoc.css` (rustdoc via
  `--extend-css`): system type for headings, Lora for text, frosted-glass chrome over a faint red glow,
  light and dark, with solid fallbacks for no backdrop blur or reduced transparency.

## Next steps

1. Choose the license: the licensing review recommends MPL-2.0 over GPL-2.0-or-later.
2. Trim the spec: a proposal cuts it from about 41 000 words to about 24 000 by moving the validation
   cases' code, the benchmark history, the loom list and the mutation table out, and dropping the
   mechanics behind the rarest rules.
3. Extend the TLA+ model to node creation, creator order and lifetimes (§16); decide whether TLC gates a
   release (§17).
4. v0.4.1: per-thread scratch buffers for a step's work lists and reuse of a run's `RunShared` (review
   2.5, 2.6), about 7 of the 10 allocations a run still makes. Measure the fast path and padding, and
   stress every test binary, on an 8–64 core machine; finer-grained sweep locking if the one sweep lock
   shows contention there (§16).
5. Decide the rerun-rate default and its configuration API (§17); the benchmark shows 1 000/s is too low.
6. Decide the v0.4.0 open questions (§17): a bound for `WHOLE_EPOCHS`, and whether lineages should be interned.
7. Make `blocked()` in `support.rs` cheap while scoped nodes exist: today `ANY_SCOPED` stays set once any
   node is scoped, and every sweep then walks handleless parents to an anchor, so a deep handleless chain
   costs quadratic time to build. A count of live scoped nodes, or a forward walk from them, would fix it.
