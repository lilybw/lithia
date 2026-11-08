# lithia implementation progress

Source of truth for resuming work. Spec: `docs/spec/LITHIA_design_spec_v0.2.5.md` (PDF alongside), which folds in
everything implementation taught; v0.2.4, the spec the implementation started from, is kept for reference.

## Layout

| Crate (dir) | Package | Role |
|---|---|---|
| `crates/core` | `lithia-core` | Engine: nodes, handles, epochs, scheduling protocol, drops, diagnostics |
| `crates/macro` | `lithia-macro` | `effect!`, `#[lithia::main]`, `#[lithia::test]` |
| `crates/public` | `lithia` | Public surface; re-exports core + macros; `lithia::test` utilities |
| `crates/validation` | `lithia-validation` | Spec §13 cases, verbatim (`tests/cases.rs`, `tests/compile.rs`) |
| `crates/test` | `lithia-test` | Edge-case unit and integration tests |
| `crates/bench` | `lithia-bench` | Spec §15 benchmarks |

## Validation cases (spec §13)

All of A–AH pass. Stress: 400/400 release runs, 300/300 debug runs, 250/250 single-core runs of A–AG.

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
| J handle lifetime | pass | AA spawn_blocking | pass |
| K generator | pass | AB SEMI_FRAGILE | pass |
| L yield before await | pass | AC ghost survivor | pass |
| M clamp via memo | pass | AD ALWAYS_PROPAGATE | pass |
| N mid-propagation creation | pass | AE panic propagates writes | pass |
| O yield nothing | pass | AF live_nodes | pass |
| P unconditional write-back | pass | AG two runtimes | pass |
| Q cut scoped to item | pass | AH must_use (trybuild) | pass |

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
| §10 Lifetime, disposal, ghosts | done | |
| §11 Concurrency, scopes | done | fast path (§11.2) left out, as v0 allows |
| §11.4 step 8 loom | done | 20 models, bound 3 by default; see "Model checking" below |
| §11.4 step 8 TLA+ | pending | open question in §17 |
| §12 Diagnostics | done | rerun threshold via `LITHIA_RERUN_RATE` (default 1000/s) |
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
- `lithia-test` holds 53 edge-case tests across values, propagation, async/generators, lifetime, capture, diagnostics and runtimes.

## Model checking (loom)

- `--cfg lithia_loom` (not `--cfg loom`, which tokio reacts to) swaps the engine's mutexes, atomics,
  thread-locals, value cells and version clock for loom's through `crates/core/src/sync.rs`, and runs
  bodies on a model executor (`crates/core/src/model.rs`) instead of tokio.
- Models live in `crates/test/tests/loom.rs`: Cases A, B, C, D, E, G, H, J, M, N, P, R, T, V, W, X,
  plus concurrent outside writes, a write during a run, disposal during a run, and a generator.
- Default preemption bound 3: ~260 000 interleavings in ~20 s. Bound 4: ~1.7 million in ~2 min, all pass.
- Teeth: reintroducing the Case J race fixed earlier makes the Case J model fail at bound 3
  (`(2, 2)` vs `(2, 1)`), matching what stress-testing found. Bound 2 was too shallow to catch it.
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

## Design migration to v0.3.0 (ownership)

- `docs/design/v0.3.0-ownership-migration.md` (also a Claude doc): stage 1 makes every node with a body
  owned by the run, lithia task or `Owner` that created it, as one more holder beside its handles;
  stage 2, automatic identity, is deferred. Revision 4, after three independent review rounds.
- Status: waiting on Lily's answers to the doc's open questions (section 12) before implementing stage 1.
  The spec stays at v0.2.5 until stage 1 passes the doc's verification plan.

## Next steps

1. A TLA+ specification of the protocol (§11.4 step 8, §17).
2. The inline fast path (§11.2), once you decide it belongs in v0 (see `docs/BENCHMARKS.md`).
3. Decide the rerun-rate default and its configuration API (§17); the benchmark shows 1 000/s is too low.
