// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Automatic capture in real code: `self`, format strings, generics and `move` (spec §4.2).

use std::sync::Arc;

use lithia::{DerivedNode, NodeValue, SourceNode, effect};

#[derive(Clone, Debug)]
struct Scaler {
    factor: u32,
}

impl Scaler {
    fn scale(&self, n: &SourceNode<u32>) -> DerivedNode<u32> {
        let n: SourceNode<u32> = n.clone();
        effect!(|n| *n.get() * self.factor)
    }

    /// `self` inline in a format string, beside a nested item and a `use` that have their own `self`.
    fn describe(&self, n: &SourceNode<u32>) -> DerivedNode<String> {
        let n: SourceNode<u32> = n.clone();
        effect!(|n| {
            use std::fmt::{self, Write as _};
            struct Twice(u32);
            impl fmt::Display for Twice {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    write!(f, "{}", self.0 * 2)
                }
            }
            let mut out: String = format!("{self:?}");
            let _ = write!(out, " {}", Twice(*n.get()));
            out
        })
    }

    fn scale_later(&self, n: &SourceNode<u32>) -> DerivedNode<u32> {
        let n: SourceNode<u32> = n.clone();
        effect!(async |n| {
            tokio::task::yield_now().await;
            *n.get() * self.factor
        })
    }
}

#[lithia::test]
async fn self_in_a_format_string_is_the_clone_and_nested_items_keep_theirs() {
    let n: SourceNode<u32> = effect!(2);
    let described: DerivedNode<String> = Scaler { factor: 10 }.describe(&n);
    lithia::test::settled(&n).await;
    assert_eq!(*described.get(), "Scaler { factor: 10 } 4");
}

#[lithia::test]
async fn self_is_captured_as_a_clone() {
    let n: SourceNode<u32> = effect!(2);
    let scaler: Scaler = Scaler { factor: 10 };
    let now: DerivedNode<u32> = scaler.scale(&n);
    let later: DerivedNode<u32> = scaler.scale_later(&n);
    let _done_with: Scaler = scaler; // the nodes hold their own clones
    lithia::test::settled(&n).await;
    assert_eq!((*now.get(), *later.get()), (20, 20));
}

#[lithia::test]
async fn inline_format_arguments_are_captured() {
    let n: SourceNode<u32> = effect!(3);
    let prefix: String = "n".to_string();
    let label: DerivedNode<String> = effect!(|n| format!("{prefix}={value}", value = n.get()));
    let width: usize = 4;
    let padded: DerivedNode<String> = effect!(|n| format!("{:>width$}", n.get()));
    lithia::flush().await;
    assert_eq!(label.get().as_str(), "n=3");
    assert_eq!(padded.get().as_str(), "   3");
    assert_eq!(prefix, "n", "the outer value stays usable");
}

fn store<K: NodeValue>(value: K) -> SourceNode<K> {
    effect!(value)
}

fn mapped<K: NodeValue + Clone + Default, F: Fn(&K) -> K + Send + Sync + Clone + 'static>(
    input: &SourceNode<K>,
    f: F,
) -> DerivedNode<K> {
    let input: SourceNode<K> = input.clone();
    effect!(|input| f(&input.get()))
}

#[lithia::test]
async fn effect_works_in_generic_functions() {
    let n: SourceNode<u32> = store(5u32);
    let plus_one: DerivedNode<u32> = mapped(&n, |v: &u32| v + 1);
    lithia::flush().await;
    assert_eq!(*plus_one.get(), 6);
}

#[lithia::test]
async fn every_body_sees_its_captures_by_reference() {
    let n: SourceNode<u32> = effect!(1);
    let offset: u32 = 10;
    let sync_body: DerivedNode<u32> = effect!(|n| {
        let seen: &u32 = offset;
        *n.get() + *seen
    });
    let moved_n: SourceNode<u32> = n.clone();
    let sync_move: DerivedNode<u32> = effect!(move |moved_n| {
        let seen: &u32 = offset;
        *moved_n.get() + *seen
    });
    let async_body: DerivedNode<u32> = effect!(async |n| {
        let seen: &u32 = offset;
        *n.get() + *seen
    });
    let generator: DerivedNode<u32> = effect!(|n| {
        let seen: &u32 = offset;
        yield *n.get() + *seen;
    });
    let block: SourceNode<u32> = effect!(async {
        let seen: &u32 = offset;
        *seen
    });
    let moved_block: SourceNode<u32> = effect!(async move {
        let owned: u32 = offset; // a block that runs once owns what it moved in
        owned
    });
    lithia::test::settled(&n).await; // every derived node here hangs off `n`
    lithia::test::settled(&block).await;
    lithia::test::settled(&moved_block).await;
    let values: [u32; 6] =
        [*sync_body.get(), *sync_move.get(), *async_body.get(), *generator.get(), *block.get(), *moved_block.get()];
    assert_eq!(values, [11, 11, 11, 11, 10, 10]);
}

#[lithia::test]
async fn move_closures_see_moved_values_by_reference() {
    let n: SourceNode<u32> = effect!(4);
    let offset: Arc<u32> = Arc::new(100);
    let moved: DerivedNode<u32> = effect!(move |n| *n.get() + **offset);
    lithia::flush().await;
    assert_eq!(*moved.get(), 104);
}

#[lithia::test]
async fn shadowing_and_closures_inside_bodies_are_not_captured() {
    let n: SourceNode<u32> = effect!(2);
    let total: DerivedNode<u32> = effect!(|n| {
        let n_value: u32 = *n.get();
        let add = |x: u32, y: u32| x + y;
        let items: Vec<u32> = (1..=n_value).collect();
        items.iter().fold(0, |acc: u32, item: &u32| add(acc, *item))
    });
    lithia::flush().await;
    assert_eq!(*total.get(), 3);
}

#[lithia::test]
async fn options_accept_bare_constants_and_paths() {
    let n: SourceNode<i32> = effect!(1, disposed = lithia::disposed::MASK);
    let m: SourceNode<i32> = effect!(1, disposed = PEND, equals = |a: &i32, b: &i32| a == b);
    lithia::dispose(&n);
    lithia::dispose(&m);
    assert_eq!((*n.get(), *m.get()), (1, 1));
}

const _LIMIT: u32 = 7;

#[lithia::test]
async fn bodies_with_constants_use_items_and_statement_macros_compile() {
    let n: SourceNode<u32> = effect!(9);
    let limited: DerivedNode<u32> = effect!(|n| (*n.get()).min(_LIMIT));
    let circle: DerivedNode<f64> = effect!(|n| {
        use std::f64::consts::PI as pi;
        f64::from(*n.get()) * pi
    });
    let pinned: DerivedNode<u32> = effect!(async |n| {
        let value: u32 = *n.get();
        tokio::pin! {
            let next = std::future::ready(value + 1);
        }
        next.await
    });
    let checked: DerivedNode<u32> = effect!(|n| {
        assert_ne!(n.get().to_string(), "{name}");
        tracing::trace!(value = %n.get(), seen = ?n.get(), "{} seen", n.get());
        *n.get()
    });
    lithia::test::settled(&n).await;
    assert_eq!(*limited.get(), 7);
    assert!((*circle.get() - 9.0 * std::f64::consts::PI).abs() < 1e-9);
    assert_eq!((*pinned.get(), *checked.get()), (10, 9));
}

#[lithia::test]
async fn a_write_inside_a_match_guard_is_a_writer_edge() {
    let seen: SourceNode<u32> = effect!(0);
    let a: SourceNode<u32> = effect!(1);
    let x: SourceNode<u32> = effect!(0);
    // The only write of `x` sits in a `matches!` guard; the edge keeps `x`'s watcher alive.
    effect!(|a| {
        let _ = matches!(Some(*a.get()), Some(v) if { x.set(v * 10); true });
    });
    effect!(|x| seen.set(*x.get()));
    lithia::flush().await;
    assert_eq!(*seen.get(), 10);
    drop(x);
    a.set(2);
    lithia::flush().await;
    assert_eq!(*seen.get(), 20);
}

/// A body passed through a `macro_rules!` macro: its captures and `yield`s resolve as they do
/// written out, though the macro's own tokens and the caller's have different hygiene.
macro_rules! derived_from {
    ($n:ident => { $($body:tt)* }) => {
        effect!(|$n| { $($body)* })
    };
}

#[lithia::test]
async fn a_body_passed_through_a_macro_captures_and_yields() {
    let n: SourceNode<u32> = effect!(2);
    let factor: u32 = 10;
    let items: DerivedNode<u32> = derived_from!(n => {
        yield *n.get() * factor;
        yield *n.get() * factor + 1;
    });
    lithia::test::settled(&n).await;
    assert_eq!(*items.get(), 21);
}
