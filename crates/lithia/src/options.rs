// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Option values for `effect!`: each option's constants live in a module of its name (spec §3.2).

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
    /// (spec §7.2). `effect!(|a, b| *a.get() + *b.get(), batches = WHOLE)`
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
    /// `effect!(|a, b| *a.get() + *b.get(), resilience = NONE)`
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
    /// (spec §10.5). `effect!(0, disposed = MASK)`
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
pub(crate) type Equals<K> = Arc<dyn Fn(&K, &K) -> bool + Send + Sync>;

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

/// Options of one node, as resolved by `effect!`.
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
    pub fn equals(mut self, equals: impl Fn(&K, &K) -> bool + Send + Sync + 'static) -> Self {
        self.equals = Some(Arc::new(equals));
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
}
