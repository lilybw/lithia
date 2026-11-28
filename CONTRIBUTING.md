# Contributing to lithia

The [design spec](https://lilybw.com/lithia/spec/) defines how lithia behaves, so most of this is about
keeping the code, the tests and the spec in step.

## How a change lands

1. Fork the repository and branch from `nightly`.
2. Open a pull request into `nightly`. CI ([`ci.yml`](.github/workflows/ci.yml)) must pass, and the
   maintainer reviews. The slower checks of [`extended.yml`](.github/workflows/extended.yml) run too but
   do not gate a merge. [`docs/VERIFICATION.md`](docs/VERIFICATION.md) lists what each one runs.
3. Once merged, the push to `nightly` is released as the next `v<version>.<n>` and the docs site is
   rebuilt. A `nightly` commit on which the extended checks have passed too is a release candidate, which
   the maintainer may publish to crates.io (below).

Changes reach `nightly` through pull requests; the maintainer may also push directly.

## Publishing to crates.io

[`publish.yml`](.github/workflows/publish.yml) publishes `lithia-macro`, then `lithate`, from a release
candidate: a `nightly` commit whose `nightly` run passed, and whose full `extended` run, cargo-mutants
included, passed too (`gh workflow run extended.yml --ref nightly` starts one). The maintainer runs it
by hand from `nightly` with the version already committed there. A version such as `1.0.0-rc.1`
publishes a release candidate, which Cargo picks only when asked for it.

- **Version.** The input confirms what is committed; it changes nothing. The run refuses unless both
  crates have that version, `lithia-macro` is pinned to it with `=`, no tag `v<version>` exists, and
  crates.io lacks it. A release also needs `CHANGELOG.md`'s section dated, the spec file of that
  version with its PDF, and the README's install line; a dry run reports those as warnings.
- **Dry run.** The default: every check, `cargo package` of both crates, the docs, and
  `cargo-semver-checks` against the last published version. Nothing is published.
- **Credentials.** The publish job runs in the `crates-io` environment, which must require the
  maintainer's approval. With `auth: trusted`, crates.io's trusted publishing gives the job a token
  that lives for the job only; set it up on crates.io for `publish.yml` and the `crates-io`
  environment once both crates exist. The first release needs `auth: token`: a token with only the
  `publish-new` and `publish-update` scopes for the two crates, stored as the environment's
  `CARGO_REGISTRY_TOKEN` secret, with its expiry date as the environment's `CRATES_IO_TOKEN_EXPIRES`
  variable. The job refuses a token that expires more than seven days ahead, or that can read
  `/api/v1/me`, which only a token not limited by scopes can; afterwards it revokes the token, so each
  token publishes once.
- **Afterwards.** The job tags `v<version>` and makes a GitHub release with both packages and the
  spec PDF. A run that failed between the two crates can be run again: a crate crates.io has is skipped.

## Layout

| Directory | Package | Holds |
|---|---|---|
| `crates/lithia` | `lithia` | The engine and its public API |
| `crates/macro` | `lithia-macro` | `node!`, `#[lithia::main]`, `#[lithia::test]` |
| `crates/validation` | `lithia-validation` | The spec's validation cases (§13), verbatim, and the compile-fail cases |
| `crates/test` | `lithia-test` | The helpers the test crates share (`src/`), the edge-case tests (`tests/edge/`, one module per area, built as one binary), the deep-chain test and the loom models (`tests/loom/`, likewise) |
| `crates/bench` | `lithia-bench` | The spec's benchmarks (§15) |
| `docs/spec` | | The design spec, current version |
| `docs/tla` | | The TLA+ models of the scheduling protocol and of node lifetimes, and their TLC models |
| `docs/site`, `.github/scripts` | | The docs site's styling, scripts and build |

Inside `crates/lithia/src`:

| Module | Holds |
|---|---|
| `api.rs`, `handles.rs`, `changes.rs`, `reference.rs`, `task.rs`, `status.rs`, `site.rs`, `options.rs`, `value.rs` | The public API |
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
cargo bench -p lithia-bench --bench scaling # only on request: shared state at 1 to N threads
RUSTFLAGS="--cfg lithia_loom" CARGO_TARGET_DIR=target/loom cargo test --release -p lithia-test --test loom
LITHIA_RANDOM_SEEDS=500 cargo test -p lithia-test --test edge --release -- random  # more random graphs
python3 .github/scripts/tlc.py MCReturn  # one TLA+ model, against its recorded state count; Java 11+
python3 .github/scripts/check.py full    # fmt, clippy, rustdoc, tests and loom, one line per step
.github/scripts/mutations.py    # each kept mutation must fail the test named for it (docs/VERIFICATION.md)
.github/scripts/stress.sh 100 50 50  # every test binary 100 times in release, 50 in debug, 50 on one core
python3 .github/scripts/site.py snapshot --version 0.0.0 --out target/site-snapshot  # needs pandoc and npx
python3 .github/scripts/mutants.py crates/lithia/src/api.rs  # cargo-mutants in place on a few files
python3 .github/scripts/spec_pdf.py  # reprints the spec's PDF; needs pandoc and Playwright
python3 .claude/tools/outline.py show crates/lithia/src --pub  # signatures and docs, no bodies
```

`CLAUDE.md` and `.claude/` hold the setup for working on lithia with Claude: agents and their
memories, skills, and the outline tool, which caches each folder's outline under `target/outline` with
a hash of its sources. `.claude/generate-outlines.py` writes outlines of the two crates to
`.claude/outlined/`, which git ignores.

Loom explores every thread interleaving up to a preemption bound: 3 by default, and
`LOOM_MAX_PREEMPTIONS=4` goes deeper in minutes rather than seconds. `LITHIA_LOOM_FAST_PATH=off` gives
every ready run a task of its own; CI runs the models both ways. `.cargo/config.toml` puts every
`#[lithia::test]` under a timeout, so a `flush` or `settled` that never returns fails its test. A change
to the scheduling protocol or to lifetimes updates its TLA+ model in [`docs/tla`](docs/tla) too.

The compile-fail cases' expected errors depend on the compiler: CI uses the toolchain in
`rust-toolchain.toml`, and
`TRYBUILD=overwrite cargo test -p lithia-validation --test compile` updates them on that version. A
change to `node!`'s expansion shows in `crates/macro/snapshots`: review the diff, then
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
