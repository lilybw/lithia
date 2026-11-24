# lithia

Thread-safe, fine-grained reactivity for Rust, after Solid 2.0's signals. One macro, `effect!`, makes
every kind of node; tokio runs them across threads.

**[Documentation](https://lilybw.com/lithia/)** · [API reference](https://lilybw.com/lithia/api/lithia/) ·
[Design spec](https://lilybw.com/lithia/spec/) · [Releases](https://github.com/lilybw/lithia/releases)

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

## What it does

- `effect!(value)` makes a source node you write to. `effect!(|a, b| …)` makes a derived node that reruns
  when `a` or `b` changes; one that returns `()` is an effect.
- Handles are `Send + Sync`, and bodies run on tokio's worker threads. Within one batch of writes,
  propagation is glitch-free: a node reached along two paths runs once, with both inputs updated.
  Batches written concurrently can still meet at such a node; `batches = WHOLE` makes it wait for both.
- Bodies can `.await` and can `yield` several values in turn. Pending and failed are states of a node,
  and a panic fails its node, not the program.
- No owners: a node lives while something outside lithia can still make it run. Nodes created inside
  a body are rebuilt on each run, or kept with `key = …`, which reruns only those whose inputs changed.
- Every release passes the tests on Linux, macOS and Windows, the loom models, and TLC on
  [TLA+ models](docs/tla) of scheduling and lifetimes. [`docs/VERIFICATION.md`](docs/VERIFICATION.md)
  lists every check.

## Using it

lithia is not on crates.io yet. Depend on the `release` branch, or pin a release tag:

```toml
[dependencies]
lithia = { git = "https://github.com/lilybw/lithia", branch = "release" }
```

It needs Rust 1.95 or newer and runs on tokio: start with `#[lithia::main]` and `#[lithia::test]`, or use
it from inside any tokio runtime. Until 1.0, the API may still change between releases.

## Releases and docs

Every push to `nightly` that passes CI becomes a release named `v<version>.<n>`, such as `v0.5.0.1`, with
the crate packages, the docs and the spec attached. `release` follows later, once a nightly release has
passed the slower checks and gone a few days without a fix. Each release's docs stay online: add
`?v=0.5.0.1` to any page under `api/` or `spec/` of the [docs site](https://lilybw.com/lithia/).

The [design spec](https://lilybw.com/lithia/spec/) is the full reference for how lithia behaves and why.
Also in the repository: [`CHANGELOG.md`](CHANGELOG.md), [`docs/known_bugs.md`](docs/known_bugs.md),
[`docs/VERIFICATION.md`](docs/VERIFICATION.md) and [`docs/BENCHMARKS.md`](docs/BENCHMARKS.md).

## Contributing

Pull requests go to `nightly`; [CONTRIBUTING.md](CONTRIBUTING.md) explains the layout, the checks and the
conventions.

## License

lithia is under the [Mozilla Public License 2.0](LICENSE). You can build on it in any project, open or
proprietary; a modified lithia you distribute keeps its own files under the same license. "lithia
Authors" in the files' SPDX headers means the people in [AUTHORS](AUTHORS). Releases up to v0.4.0 were
under GPL-2.0-or-later. The Lora font files in `docs/site/fonts` are under the SIL Open Font License 1.1.

This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
