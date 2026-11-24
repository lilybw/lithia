# Contributing to lithia

The [design spec](https://lilybw.com/lithia/spec/) defines how lithia behaves, so most of this is about
keeping the code, the tests and the spec in step.

## How a change lands

1. Fork the repository and branch from `nightly`.
2. Open a pull request into `nightly`. CI ([`ci.yml`](.github/workflows/ci.yml)) must pass, and the
   maintainer reviews. The slower checks of [`extended.yml`](.github/workflows/extended.yml) run too but
   do not gate a merge. [`docs/VERIFICATION.md`](docs/VERIFICATION.md) lists what each one runs.
3. Once merged, the push to `nightly` is released as the next `v<version>.<n>` and the docs site is
   rebuilt. `release` later moves to that release's commit, once the extended checks have passed on it
   and it has gone a few days on `nightly` without a fix on top.

Changes reach `nightly` and `release` through pull requests; the maintainer may also push directly.

## Layout

| Directory | Package | Holds |
|---|---|---|
| `crates/lithia` | `lithia` | The engine and its public API |
| `crates/macro` | `lithia-macro` | `effect!`, `#[lithia::main]`, `#[lithia::test]` |
| `crates/validation` | `lithia-validation` | The spec's validation cases (§13), verbatim, and the compile-fail cases |
| `crates/test` | `lithia-test` | The helpers the test crates share (`src/`), the edge-case tests (`tests/edge/`, one module per area, built as one binary), the deep-chain test and the loom models (`tests/loom/`, likewise) |
| `crates/bench` | `lithia-bench` | The spec's benchmarks (§15) |
| `docs/spec` | | The design spec, current version |
| `docs/tla` | | The TLA+ models of the scheduling protocol and of node lifetimes, and their TLC models |
| `docs/site`, `.github/scripts` | | The docs site's styling, scripts and build |

Inside `crates/lithia/src`:

| Module | Holds |
|---|---|
| `api.rs`, `handles.rs`, `changes.rs`, `status.rs`, `site.rs`, `options.rs`, `value.rs` | The public API |
| `expand/` | What the macro's expansions call: creation, keyed reclaim, captures, the hidden traits, the entry points |
| `graph/` | The node record: ids and the clock, header, scheduler state, sets, epochs |
| `protocol/` | The scheduling protocol (§7, §11.4): work done after unlocking, propagation, delivery, the run lifecycle, linking, defers |
| `run/` | A run's future and control block, the yielder, tasks |
| `lifetime/` | Operations, support and sweeps, disposal, teardown, deferred drops (§10) |
| `exec/` | Spawning, the fast path, and the loom executor |
| `testing/` | What `lithia::test` reads: settledness, its trace of created and written nodes, progress |
| `context.rs`, `diag.rs`, `sync.rs` | The run context, diagnostics, and the lock and atomic types every module uses |

## Commands

```sh
cargo test --workspace          # everything, including the compile-fail cases
cargo test -p lithia-validation # the spec's cases
cargo test -p lithia-test --test edge  # the edge-case tests
cargo bench -p lithia-bench     # prints a results table; LITHIA_BENCH=spawn,clock runs only those rows
cargo bench -p lithia-bench --bench allocs  # heap allocations per run, per write and per node
RUSTFLAGS="--cfg lithia_loom" CARGO_TARGET_DIR=target/loom cargo test --release -p lithia-test --test loom
LITHIA_RANDOM_SEEDS=500 cargo test -p lithia-test --test edge --release -- random  # more random graphs
(cd docs/tla/models && ../tlc.sh MCReturn)  # one TLA+ model with TLC; needs Java 11+
.github/scripts/mutations.py    # each kept mutation must fail the test named for it (docs/VERIFICATION.md)
.github/scripts/stress.sh 100 50 50  # every test binary 100 times in release, 50 in debug, 50 on one core
python3 .github/scripts/site.py snapshot --version 0.0.0 --out target/site-snapshot  # needs pandoc and npx
```

Loom explores every thread interleaving up to a preemption bound: 3 by default, and
`LOOM_MAX_PREEMPTIONS=4` goes deeper in minutes rather than seconds. `LITHIA_LOOM_FAST_PATH=off` gives
every ready run a task of its own; CI runs the models both ways. `.cargo/config.toml` puts every
`#[lithia::test]` under a timeout, so a `flush` or `settled` that never returns fails its test. A change
to the scheduling protocol or to lifetimes updates its TLA+ model in [`docs/tla`](docs/tla) too.

The compile-fail cases' expected errors depend on the compiler: CI uses the toolchain in
`rust-toolchain.toml`, and
`TRYBUILD=overwrite cargo test -p lithia-validation --test compile` updates them on that version. A
change to `effect!`'s expansion shows in `crates/macro/snapshots`: review the diff, then
`LITHIA_SNAPSHOTS=overwrite cargo test -p lithia-macro` updates them.

## Conventions

- Every binding and closure parameter has a type annotation.
- Doc comments are at most two lines, in the standard library's voice, with an example for generic APIs;
  the reasoning behind a design belongs in the spec.
- Behaviour changes update the spec and `CHANGELOG.md`, with a validation case that pins the behaviour
  down. Until 1.0 a new spec version replaces the file in `docs/spec`, and git history keeps the old
  ones; from 1.0 each released version keeps its own file.
- `cargo fmt`, and clippy without warnings (`-D warnings`), in both the normal and the loom build.
- New source files start with the two-line SPDX header the others have:

  ```rust
  // SPDX-FileCopyrightText: 2026 lithia Authors
  // SPDX-License-Identifier: MPL-2.0
  ```

## License

Contributions are made under the project's license, MPL-2.0. Add yourself to
[AUTHORS](AUTHORS) in your first pull request.
