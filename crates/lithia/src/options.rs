// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Option values for `node!`: each option's constants live in a module of its name (spec §3.2).

use std::any::TypeId;
use std::marker::PhantomData;
use std::sync::Arc;

use crate::NodeValue;
use crate::expand::keyed::{Inputs, Keyed, NodeKey, Reclaim};

/// Defines an option's module: its opaque policy type, the constants that are its only values, and
/// a `Debug` that prints the constant's path.
macro_rules! policy {
    (
        $(#[$module_doc:meta])*
        mod $module:ident;
        $(#[$type_doc:meta])*
        struct $name:ident($inner:ident) default $default:ident;
        $($(#[$const_doc:meta])* const $constant:ident = $variant:ident;)+
    ) => {
        $(#[$module_doc])*
        pub mod $module {
            $(#[$type_doc])*
            #[derive(Clone, Copy, PartialEq, Eq, Hash)]
            pub struct $name(pub(crate) $inner);

            #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
            pub(crate) enum $inner {
                $($variant,)+
            }

            $($(#[$const_doc])* pub const $constant: $name = $name($inner::$variant);)+

            impl Default for $name {
                fn default() -> Self {
                    $default
                }
            }

            impl std::fmt::Debug for $name {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    let name: &str = match self.0 {
                        $($inner::$variant => concat!(stringify!($module), "::", stringify!($constant)),)+
                    };
                    f.write_str(name)
                }
            }
        }
    };
}

policy! {
    /// Values of the `batches` option: whether a derived node runs on partly propagated batches
    /// (spec §7.2). `node!(|a, b| *a.get() + *b.get(), batches = WHOLE)`
    mod batches;
    /// When a derived node runs while batches are still reaching it; one of the constants beside it.
    struct Batches(Runs) default PARTIAL;
    /// Runs once every batch that has reached the node is ready at it, never on one batch's changes
    /// beside another's half-propagated ones.
    const WHOLE = OnWhole;
    /// Runs as soon as a batch is ready at the node, whatever other batches are on their way. The default.
    const PARTIAL = OnPartial;
}

policy! {
    /// Values of the `resilience` option: whether failed parents fail a derived node (spec §9.2).
    /// `node!(|a, b| *a.get() + *b.get(), resilience = NONE)`
    mod resilience;
    /// How many failed parents a derived node withstands; one of the constants beside it.
    struct Resilience(Fails) default COMPLETE;
    /// Never fails through its parents. The default.
    const COMPLETE = Never;
    /// Fails once all of its parents are failed.
    const PARTIAL = WhenAll;
    /// Fails once any parent is failed.
    const NONE = WhenAny;
}

policy! {
    /// Values of the `disposed` option: what readers and children see once a node is disposed
    /// (spec §10.5). `node!(0, disposed = MASK)`
    mod disposed;
    /// What a disposed node shows; one of the constants beside it.
    struct Disposal(Ghost) default FAIL;
    /// `get` panics, children see a failed parent, and the value is dropped at disposal. The default.
    const FAIL = Fail;
    /// `get` returns the last value, and children see a ready parent, frozen.
    const MASK = Mask;
    /// `get` returns the last value, and children see a parent pending forever.
    const PEND = Pend;
}

/// Comparator for the `equals` option, where `true` means equal.
#[doc(hidden)]
pub struct Equals<K>(Compare<K>);

/// A comparator closure, shared by a node and its comparisons.
type Closure<K> = Arc<dyn Fn(&K, &K) -> bool + Send + Sync>;

/// A comparator: `PartialEq` through a plain function, or a closure that may own data.
enum Compare<K> {
    Plain(fn(&K, &K) -> bool),
    /// The closure, and whether dropping it may run user code.
    Shared(Closure<K>, bool),
}

impl<K> Clone for Equals<K> {
    fn clone(&self) -> Self {
        Equals(match &self.0 {
            Compare::Plain(f) => Compare::Plain(*f),
            Compare::Shared(f, glue) => Compare::Shared(Arc::clone(f), *glue),
        })
    }
}

impl<K: 'static> Equals<K> {
    /// A comparator from a plain function, which costs nothing to keep or drop.
    pub(crate) fn plain(compare: fn(&K, &K) -> bool) -> Self {
        Equals(Compare::Plain(compare))
    }

    /// A comparator from a closure.
    pub(crate) fn shared<F: Fn(&K, &K) -> bool + Send + Sync + 'static>(compare: F) -> Self {
        Equals(Compare::Shared(Arc::new(compare), std::mem::needs_drop::<F>()))
    }

    /// Whether `old` and `new` count as equal.
    pub(crate) fn equal(&self, old: &K, new: &K) -> bool {
        match &self.0 {
            Compare::Plain(f) => f(old, new),
            Compare::Shared(f, _) => f(old, new),
        }
    }

    /// Takes the comparator out of `slot`, leaving one that compares nothing.
    pub(crate) fn take(slot: &mut Equals<K>) -> Equals<K> {
        std::mem::replace(slot, Equals::plain(|_: &K, _: &K| -> bool { false }))
    }

    /// Drops the comparator: off the protocol if dropping it may run user code, here otherwise (spec §10.3).
    pub(crate) fn release(self, runtime: Option<&tokio::runtime::Handle>) {
        if let Compare::Shared(_, true) = self.0 {
            crate::lifetime::drops::drop_later(self, runtime);
        }
    }
}

/// The options that shape how a node behaves: part of a keyed node's identity (spec §10.1).
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shape {
    pub(crate) batches: batches::Batches,
    pub(crate) resilience: resilience::Resilience,
    pub(crate) disposed: disposed::Disposal,
}

impl Shape {
    /// Starts from the defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the `batches` option.
    pub fn batches(mut self, batches: batches::Batches) -> Self {
        self.batches = batches;
        self
    }

    /// Sets the `resilience` option.
    pub fn resilience(mut self, resilience: resilience::Resilience) -> Self {
        self.resilience = resilience;
        self
    }

    /// Sets the `disposed` option.
    pub fn disposed(mut self, disposed: disposed::Disposal) -> Self {
        self.disposed = disposed;
        self
    }

    /// Whether the node runs on whole batches only.
    pub(crate) fn whole_batches(&self) -> bool {
        self.batches == batches::WHOLE
    }

    /// Whether, once disposed, the node fails its readers and children and drops its value.
    pub(crate) fn ghost_fails(&self) -> bool {
        self.disposed == disposed::FAIL
    }
}

/// Options of one node, as resolved by `node!`.
#[doc(hidden)]
pub struct Options<K> {
    pub(crate) equals: Option<Equals<K>>,
    pub(crate) shape: Shape,
    pub(crate) keyed: Option<Keyed>,
}

impl<K> Options<K> {
    /// Starts from `shape`, with no comparator and no key.
    pub fn new(shape: Shape) -> Self {
        Options { equals: None, shape, keyed: None }
    }

    /// Sets the comparator; `true` means equal.
    pub fn equals(mut self, equals: impl Fn(&K, &K) -> bool + Send + Sync + 'static) -> Self
    where
        K: 'static,
    {
        self.equals = Some(Equals::shared(equals));
        self
    }

    /// Sets the key, with what the lookup by key found and the captures to keep (spec §4.7).
    pub fn keyed<T>(mut self, reclaim: Reclaim<K>, key: NodeKey<T>, inputs: Inputs) -> Self
    where
        K: NodeValue,
    {
        self.keyed = Some(Keyed::new(reclaim, key, inputs));
        self
    }

    /// Takes the comparator `pick` chooses by type when no `equals` was given (spec §6.4).
    pub(crate) fn or_implicit(mut self, pick: impl FnOnce(Implicit<K>) -> Option<Equals<K>>) -> Self {
        let implicit: Option<Equals<K>> = pick(Implicit(PhantomData));
        if self.equals.is_none() {
            self.equals = implicit;
        }
        self
    }
}

/// What `node!` probes, once the value type is known, for the comparator a node takes by default (spec §6.4).
#[doc(hidden)]
pub struct Implicit<K>(PhantomData<fn() -> K>);

/// A value type `node!` compares with `PartialEq` unless told otherwise (spec §6.4).
#[doc(hidden)]
pub trait EqualsByDefault: PartialEq + NodeValue {}

impl<T: PartialEq + NodeValue> EqualsByDefault for T {}

/// The default comparator of a type with `PartialEq`: `PartialEq`, except for `()` (spec §6.4).
#[doc(hidden)]
pub trait ImplicitViaEq<K> {
    /// The comparator, if any.
    fn __lithia_implicit(&self) -> Option<Equals<K>>;
}

impl<K: EqualsByDefault> ImplicitViaEq<K> for &Implicit<K> {
    fn __lithia_implicit(&self) -> Option<Equals<K>> {
        // An effect's `()` is always equal to the last one, yet each run of it is a change.
        if TypeId::of::<K>() == TypeId::of::<()>() {
            return None;
        }
        Some(Equals::plain(|old: &K, new: &K| -> bool { old == new }))
    }
}

/// The default comparator of any other type: none, so every value propagates (spec §6.4).
#[doc(hidden)]
pub trait ImplicitViaNone<K> {
    /// No comparator.
    fn __lithia_implicit(&self) -> Option<Equals<K>>;
}

impl<K> ImplicitViaNone<K> for Implicit<K> {
    fn __lithia_implicit(&self) -> Option<Equals<K>> {
        None
    }
}

/// No comparator by default: what `no_equals`, or an explicit `equals`, expands to (spec §6.4).
#[doc(hidden)]
pub fn no_implicit<K>(_: Implicit<K>) -> Option<Equals<K>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_comparator_is_a_plain_function() {
        // A closure would cost every node an allocation, and every freed node a deferred drop.
        let picked: Option<Equals<u64>> = (&&Implicit::<u64>(PhantomData)).__lithia_implicit();
        assert!(matches!(picked, Some(Equals(Compare::Plain(_)))));
        assert!(picked.is_some_and(|eq: Equals<u64>| eq.equal(&1, &1) && !eq.equal(&1, &2)));
    }

    #[test]
    fn only_a_closure_that_owns_data_drops_off_the_protocol() {
        let plain: Equals<u64> = Equals::shared(|a: &u64, b: &u64| a == b);
        assert!(matches!(plain.0, Compare::Shared(_, false)));
        let owned: Vec<u64> = vec![1];
        let owning: Equals<u64> = Equals::shared(move |a: &u64, b: &u64| owned.contains(a) && a == b);
        assert!(matches!(owning.0, Compare::Shared(_, true)));
    }
}
