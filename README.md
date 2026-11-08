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

The design is specified in [`docs/spec`](docs/spec/LITHIA_design_spec_v0.2.4.pdf); implementation status
and decisions are tracked in [`docs/PROGRESS.md`](docs/PROGRESS.md).

## Crates

| Directory | Package | Role |
|---|---|---|
| `crates/public` | `lithia` | The public API; depend on this one |
| `crates/core` | `lithia-core` | Engine internals |
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

`.cargo/config.toml` runs every `#[lithia::test]` under a timeout (`LITHIA_TEST_TIMEOUT_MS`), so a
`flush` or `settled` that never returns fails its test instead of stalling the run.
