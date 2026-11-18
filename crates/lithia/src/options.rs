// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Typed option constants for `effect!` (spec §3.2).

use std::sync::Arc;

use crate::NodeValue;
use crate::keyed::{Inputs, Keyed, NodeKey, Reuse};

/// How a derived node reacts to failed parents (spec §9.2); not set means never.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fragility {
    /// The node fails when any parent is failed.
    Fragile,
    /// The node fails when all parents are failed.
    SemiFragile,
}

/// Fails the node when any parent is failed.
pub const FRAGILE: Fragility = Fragility::Fragile;
/// Fails the node when all parents are failed.
pub const SEMI_FRAGILE: Fragility = Fragility::SemiFragile;

/// What readers and children see once a node is disposed (spec §10.5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GhostPolicy {
    /// `get` panics, children see a failed parent, and the value is dropped at disposal.
    #[default]
    CauseFailure,
    /// `get` returns the last value, and children see a ready, frozen parent.
    EmitLatest,
    /// `get` returns the last value, and children see a parent pending forever.
    SetAsPending,
}

/// `get` panics on the ghost; children see a failed parent. The default.
pub const ON_GHOST_CAUSE_FAILURE: GhostPolicy = GhostPolicy::CauseFailure;
/// `get` returns the last value; children see a ready parent.
pub const ON_GHOST_EMIT_LATEST: GhostPolicy = GhostPolicy::EmitLatest;
/// `get` returns the last value; children see a parent pending forever.
pub const ON_GHOST_SET_AS_PENDING: GhostPolicy = GhostPolicy::SetAsPending;

bitflags::bitflags! {
    /// Behaviour flags for a node, combinable with `|` (spec §3.2).
    /// `effect!(0, equals = lithia::eq, flags = ALWAYS_PROPAGATE)`
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct Flags: u32 {
        /// Keeps the old value on an equal write, but propagates anyway; without `equals`, a no-op.
        const ALWAYS_PROPAGATE = 1;
        /// Runs only once every epoch that has reached the node is ready at it, so a run never
        /// reads one epoch's changes beside another's half-propagated ones.
        const WHOLE_EPOCHS = 2;
    }
}

/// Keeps the old value on an equal write, but propagates anyway; without `equals`, a no-op.
pub const ALWAYS_PROPAGATE: Flags = Flags::ALWAYS_PROPAGATE;
/// Runs only once every epoch that has reached the node is ready at it (spec §8).
pub const WHOLE_EPOCHS: Flags = Flags::WHOLE_EPOCHS;

/// Comparator for the `equals` option, where `true` means equal.
pub(crate) type Equals<K> = Arc<dyn Fn(&K, &K) -> bool + Send + Sync>;

/// The options that shape how a node behaves: part of a keyed node's identity (spec §10.1).
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shape {
    pub(crate) fragility: Option<Fragility>,
    pub(crate) ghost: GhostPolicy,
    pub(crate) flags: Flags,
}

impl Shape {
    /// Starts from the defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the fragility.
    pub fn fragility(mut self, fragility: Fragility) -> Self {
        self.fragility = Some(fragility);
        self
    }

    /// Sets the ghost policy.
    pub fn ghost(mut self, ghost: GhostPolicy) -> Self {
        self.ghost = ghost;
        self
    }

    /// Sets the flags.
    pub fn flags(mut self, flags: Flags) -> Self {
        self.flags = flags;
        self
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
    pub fn keyed<T>(mut self, reuse: Reuse<K>, key: NodeKey<T>, inputs: Inputs) -> Self
    where
        K: NodeValue,
    {
        self.keyed = Some(Keyed::new(reuse, key, inputs));
        self
    }
}
