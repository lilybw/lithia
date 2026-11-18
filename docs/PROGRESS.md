# lithia implementation progress

Source of truth for resuming work. Spec: `docs/spec/LITHIA_design_spec_v0.3.1.md` (PDF alongside), which adds keyed
nodes to v0.3.0's lifetime rule; v0.3.0, v0.2.5 and v0.2.4 are kept for reference.

## Layout

| Crate (dir) | Package | Role |
|---|---|---|
| `crates/lithia` | `lithia` | Engine and public API: nodes, handles, epochs, scheduling protocol, drops, diagnostics; re-exports the macros; `lithia::test` utilities |
| `crates/macro` | `lithia-macro` | `effect!`, `#[lithia::main]`, `#[lithia::test]` |
| `crates/validation` | `lithia-validation` | Spec §13 cases, verbatim (`tests/cases.rs` A–AG, `tests/support.rs` AI–BC, `tests/keyed.rs` BD–BK, `tests/compile.rs` AH) |
| `crates/test` | `lithia-test` | Edge-case tests: `tests/edge/` (one module per area, one binary), `tests/deep_chain.rs`, `tests/loom.rs` |
| `crates/bench` | `lithia-bench` | Spec §15 benchmarks |

## Validation cases (spec §13)

All of A–BK pass (63 cases). Stress for v0.3.0: every test binary (cases, both `support` files, and the
seven other `lithia-test` files) 400/400 release runs, 200/200 debug runs, 250/250 single-core runs.
Stress for v0.3.1: all 12 binaries, both `keyed` files included, 100/100 release, 50/50 debug and 60/60
single-core runs; 155 workspace tests; 35 loom models at bounds 3 and 4.

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
| §11 Concurrency, scopes | done | fast path (§11.2) left out, as v0 allows |
| §11.4 step 8 loom | done | 35 models, bound 3 by default; see "Model checking" below |
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
- `lithia-test` holds 75 edge-case tests: 74 in `tests/edge/` (values, propagation, async/generators, lifetime,
  capture, diagnostics, runtimes, support, keyed nodes; one module each, sharing one tracing collector in
  `events.rs`) and the 20 000-node chain in `tests/deep_chain.rs`. The chain runs alone because a sweep
  through handleless nodes walks to the top of the chain while any scoped node is alive, which makes
  building it beside other tests' scoped nodes quadratic (see Next steps).

## Model checking (loom)

- `--cfg lithia_loom` (not `--cfg loom`, which tokio reacts to) swaps the engine's mutexes, atomics,
  thread-locals, value cells and version clock for loom's through `crates/lithia/src/sync.rs`, and runs
  bodies on a model executor (`crates/lithia/src/model.rs`) instead of tokio.
- Models live in `crates/test/tests/loom.rs`: Cases A, B, C, D, E, G, H, J, M, N, P, R, T, V, W, X,
  plus concurrent outside writes, a write during a run, disposal during a run, and a generator; and since
  v0.3.0 twelve lifetime models (spec §11.4), which assert on `ModelScope::live()`; v0.3.1 adds three for keyed
  nodes (a reclaim racing a last-handle drop, a disposal during a reclaim, a supersede handing keyed nodes on).
- Default preemption bound 3: all 35 explore ~340 000 interleavings in ~31 s on two test threads. Bound 4:
  ~4.7 million in ~7 min, all pass.
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

1. Merge `release` into `main`; choose a license before making the repo public (`TODO.md`).
2. A TLA+ specification of the protocol and of lifetimes (§11.4 step 8, §16, §17), which the spec asks for before shipping.
3. The inline fast path (§11.2), once you decide it belongs in v0 (see `docs/BENCHMARKS.md`).
4. Decide the rerun-rate default and its configuration API (§17); the benchmark shows 1 000/s is too low.
5. Decide the keyed open questions (§17): options of a later creation, and whether duplicate keys warn.
6. Make `blocked()` in `support.rs` cheap while scoped nodes exist: today `ANY_SCOPED` stays set once any
   node is scoped, and every sweep then walks handleless parents to an anchor, so a deep handleless chain
   costs quadratic time to build. A count of live scoped nodes, or a forward walk from them, would fix it.
