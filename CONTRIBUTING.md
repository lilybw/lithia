# Contributing to lithia

Thank you for helping. lithia is small and strict about behaving as its
[design spec](https://lilybw.com/lithia/spec/) says, so most of this is about keeping the code, the
tests and the spec in step.

## How a change lands

1. Fork the repository and branch from `nightly`.
2. Open a pull request into `nightly`. CI ([`ci.yml`](.github/workflows/ci.yml)) runs on Ubuntu and
   Windows: format, lints (also under loom), docs, the tests with the validation and compile-fail cases,
   and the loom models. All of it must pass, and the maintainer reviews.
3. Once merged, the push to `nightly` is released as the next `v<version>.<n>` and the docs site is
   rebuilt. `release` takes stable merges from `nightly`.

Only pull requests change `nightly` and `release`; the maintainer can also push to them directly.

## Layout

| Directory | Package | Holds |
|---|---|---|
| `crates/lithia` | `lithia` | The engine and its public API |
| `crates/macro` | `lithia-macro` | `effect!`, `#[lithia::main]`, `#[lithia::test]` |
| `crates/validation` | `lithia-validation` | The spec's validation cases (§13), verbatim, and the compile-fail cases |
| `crates/test` | `lithia-test` | Edge-case tests (`tests/edge/`, one module per area, built as one binary), the deep-chain test and the loom models |
| `crates/bench` | `lithia-bench` | The spec's benchmarks (§15) |
| `docs/spec` | | The design spec, one file per version |
| `docs/site`, `.github/scripts` | | The docs site's styling, scripts and build |

## Commands

```sh
cargo test --workspace          # everything, including the compile-fail cases
cargo test -p lithia-validation # the spec's cases
cargo test -p lithia-test --test edge  # the edge-case tests
cargo bench -p lithia-bench     # prints a results table
RUSTFLAGS="--cfg lithia_loom" CARGO_TARGET_DIR=target/loom cargo test --release -p lithia-test --test loom
python3 .github/scripts/site.py snapshot --version 0.0.0 --out target/site-snapshot  # needs pandoc and npx
```

Loom explores every thread interleaving of the scheduling protocol up to a preemption bound (3 by
default; `LOOM_MAX_PREEMPTIONS=4` goes deeper, in minutes rather than seconds). `.cargo/config.toml` runs
every `#[lithia::test]` under a timeout, so a `flush` or `settled` that never returns fails its test.

The compile-fail cases' expected errors depend on the compiler: CI uses rustc 1.97.0, and
`TRYBUILD=overwrite cargo test -p lithia-validation --test compile` updates them on that version.

## Conventions

- Every binding and closure parameter has a type annotation.
- Doc comments are at most two lines, in the standard library's voice, with an example for generic APIs;
  the reasoning behind a design belongs in the spec.
- Behaviour changes update the spec: a new version is a new file in `docs/spec`, with its changes listed
  at the top, and a validation case that pins the behaviour down.
- `cargo fmt`, and clippy without warnings (`-D warnings`), in both the normal and the loom build.
- New source files start with the two-line SPDX header the others have:

  ```rust
  // SPDX-FileCopyrightText: 2026 lithia Authors
  // SPDX-License-Identifier: GPL-2.0-or-later
  ```

## License

Contributions are made under the project's license, GPL-2.0-or-later. Add yourself to
[AUTHORS](AUTHORS) in your first pull request.
