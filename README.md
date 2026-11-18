# lithia

A thread-safe, fine-grained reactive engine for Rust on tokio, bringing the core of Solid 2.0's
signal model to Rust through one primitive: the `effect!` macro.

```rust
use lithia::{DerivedNode, SourceNode, effect};

#[lithia::main]
async fn main() {
    let count: SourceNode<u32> = effect!(0);                          // source node
    let doubled: DerivedNode<u32> = effect!(|count| *count.get() * 2); // derived node
    let _log: DerivedNode<()> = effect!(|doubled| println!("{}", doubled.get())); // effect
    count.set(3);
    lithia::flush().await;
    assert_eq!(*doubled.get(), 6);
}
```

The design is specified in [`docs/spec`](docs/spec/LITHIA_design_spec_v0.3.1.md) (v0.3.1, also as PDF); implementation status
and decisions are tracked in [`docs/PROGRESS.md`](docs/PROGRESS.md).

Requires Rust 1.95 or newer (edition 2024).

## Crates

| Directory | Package | Role |
|---|---|---|
| `crates/lithia` | `lithia` | The engine and its public API; depend on this one |
| `crates/macro` | `lithia-macro` | `effect!`, `#[lithia::main]`, `#[lithia::test]` |
| `crates/validation` | `lithia-validation` | The spec's validation cases, verbatim |
| `crates/test` | `lithia-test` | Edge-case unit and integration tests |
| `crates/bench` | `lithia-bench` | The spec's §15 benchmarks |

## Commands

```sh
cargo test --workspace        # everything, including compile-fail tests
cargo test -p lithia-validation
cargo bench -p lithia-bench   # prints a results table
```

Model checking with [loom](https://docs.rs/loom) explores every thread interleaving of the
scheduling protocol over the validation cases, up to a preemption bound (default 3):

```sh
RUSTFLAGS="--cfg lithia_loom" CARGO_TARGET_DIR=target/loom cargo test --release -p lithia-test --test loom
LOOM_MAX_PREEMPTIONS=4 ...    # deeper: about 7 minutes instead of 30 seconds
```

`.cargo/config.toml` runs every `#[lithia::test]` under a timeout (`LITHIA_TEST_TIMEOUT_MS`), so a
`flush` or `settled` that never returns fails its test instead of stalling the run.

## Releases

Every push to `nightly` runs the full test matrix on Ubuntu and Windows
([`ci.yml`](.github/workflows/ci.yml): format, lints, docs, tests, validation and compile-fail cases, loom
models). When it passes, [`nightly.yml`](.github/workflows/nightly.yml) releases that commit as
`v<crate version>.<n>`, such as `v0.3.1.1`, `v0.3.1.2`, with the `lithia` and `lithia-macro` crate
packages, the API docs, the design spec and SHA-256 sums attached. A new crate version in `Cargo.toml`
starts again at `.1`. The `release` branch holds what was last merged as stable.
