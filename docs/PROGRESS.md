# lithia implementation progress

Source of truth for resuming work. Spec: `docs/spec/LITHIA_design_spec_v0.3.0.md` (PDF alongside), which folds in
the v0.3.0 lifetime migration and everything its implementation taught; v0.2.5 and v0.2.4 are kept for reference.

## Layout

| Crate (dir) | Package | Role |
|---|---|---|
| `crates/lithia` | `lithia` | Engine and public API: nodes, handles, epochs, scheduling protocol, drops, diagnostics; re-exports the macros; `lithia::test` utilities |
| `crates/macro` | `lithia-macro` | `effect!`, `#[lithia::main]`, `#[lithia::test]` |
| `crates/validation` | `lithia-validation` | Spec §13 cases, verbatim (`tests/cases.rs` A–AG, `tests/support.rs` AI–BC, `tests/compile.rs` AH) |
| `crates/test` | `lithia-test` | Edge-case unit and integration tests |
| `crates/bench` | `lithia-bench` | Spec §15 benchmarks |

## Validation cases (spec §13)

All of A–BC pass (55 cases). Stress for v0.3.0: every test binary (cases, both `support` files, and the
seven other `lithia-test` files) 400/400 release runs, 200/200 debug runs, 250/250 single-core runs.

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

## Spec sections

| Section | Status | Notes |
|---|---|---|
| §3 Public API | done | all handles, views, traits, free functions, `lithia::test` |
| §4.1–4.5 effect! expansion | done | deps, capture, site, init, generator lowering |
| §4.6 compile errors | done | every message covered by `tests/ui` (trybuild) |
| §5 Node model | done | |
| §6 Writes, batching, defer, equals | done | |
| §7 Propagation | done | see protocol notes below |
| §8 Pending/async/generators | done | |
| §9 Failure, fragility | done | |
| §10 Lifetime, disposal, ghosts | done | v0.3.0: generations, support, sweeps, `.scoped()` (`crates/lithia/src/support.rs`, `capture.rs`) |
| §11 Concurrency, scopes | done | fast path (§11.2) left out, as v0 allows |
| §11.4 step 8 loom | done | 32 models, bound 3 by default; see "Model checking" below |
| §11.4 step 8 TLA+ | pending | open question in §17 |
| §12 Diagnostics | done | rerun threshold via `LITHIA_RERUN_RATE` (default 1000/s); first-run, growth and unread-memo warnings |
| §15 Benchmarks | done | results and observations in `docs/BENCHMARKS.md` |

## Decisions and interpretations

- Validation cases live in the `lithia-validation` crate rather than a `lithia::validation` module, per the six-module layout.
- Test harness timeout: `.cargo/config.toml` sets `LITHIA_TEST_TIMEOUT_MS`, which `#[lithia::test]` honours.
- A node owes its children at most one release per epoch (an open entry, or a run holding the epoch unreleased); a run's release defers into an open entry of the same epoch. This keeps joins glitch-free when a writer is re-framed mid-run (Case T).
- New nodes do not run until their creation is delivered (seeded); entries opened at link time wait for the seed (Case N runs `m` once).
- `settled(&node)` waits on the node, its descendants and nodes created by their runs (needed for Case U).
- `defer` holds a token on the caller's scope until its task delivers, so `flush()` also waits for deferred work.
- A write equal under `ALWAYS_PROPAGATE` counts as a run (supersedes an async block feeding the source).
- Non-`Clone` captures are a compile error with a hint to wrap in `Arc` (spec §4.2 "wrongly captured … fails to compile").
- `move` async closures and generators move their captures into the shared `Arc<Captures>` instead of cloning.
- Run tasks drop every strong reference to their node before closing epoch tokens, so a settled scope never sees a node kept alive by lithia (found by stress-testing Case J).

- Reflow of a source node re-emits its value to its children but sends nothing to `changes()`, since the value did not change.
- Diagnostics raised while a scheduling lock is held are gathered and emitted after it is released (spec §11.4 step 1).
- `lithia-test` holds 65 edge-case tests across values, propagation, async/generators, lifetime, capture, diagnostics, runtimes and support (`tests/support.rs`).

## Model checking (loom)

- `--cfg lithia_loom` (not `--cfg loom`, which tokio reacts to) swaps the engine's mutexes, atomics,
  thread-locals, value cells and version clock for loom's through `crates/lithia/src/sync.rs`, and runs
  bodies on a model executor (`crates/lithia/src/model.rs`) instead of tokio.
- Models live in `crates/test/tests/loom.rs`: Cases A, B, C, D, E, G, H, J, M, N, P, R, T, V, W, X,
  plus concurrent outside writes, a write during a run, disposal during a run, and a generator; and since
  v0.3.0 twelve lifetime models (spec §11.4), which assert on `ModelScope::live()`.
- Default preemption bound 3: all 32 explore ~310 000 interleavings in ~30 s on two test threads. Bound 4:
  ~4.4 million in ~7 min, all pass.
- Sweeps use ordered maps and loom builds always take the scoped path, so models stay deterministic when
  several run in one process.
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

- Builds and tests run in a cloud container; the user's repo `E:\GitHub\lithia` is synced by sending a
  git bundle with a unique file name per sync (the transfer reuses content for a repeated path), then a
  fast-forward merge there. GitHub `origin` is `lilybw/lithia` (private). Commits are authored as
  `claudia <lilybwanscher@gmail.com>`.
- Stress loop used before every sync: run each test binary 100–400 times in debug and release, and once
  pinned to a single core with `taskset -c 0`.

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

## Next steps

1. A TLA+ specification of the protocol and of lifetimes (§11.4 step 8, §16, §17).
2. The inline fast path (§11.2), once you decide it belongs in v0 (see `docs/BENCHMARKS.md`).
3. Decide the rerun-rate default and its configuration API (§17); the benchmark shows 1 000/s is too low.
