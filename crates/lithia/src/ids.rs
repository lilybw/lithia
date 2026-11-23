// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Process-wide identities and the version clock (spec §5.1, §11.4 step 5).

use std::fmt;
use std::marker::PhantomData;
use std::num::NonZeroU64;
use std::sync::atomic::Ordering;

use crate::sync::Padded;

/// A value stored in an [`Atomic`] as its bits.
pub(crate) trait Bits: Copy {
    fn to_bits(self) -> u64;
    fn from_bits(bits: u64) -> Self;
}

/// Process-unique node identity in creation order; creation order is topological.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(NonZeroU64);

/// Process-unique epoch identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct EpochId(u64);

/// Process-unique identity of a scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ScopeId(u64);

/// A reading of the version clock; a later change carries a larger stamp.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Stamp(u64);

/// Counts a node's runs; the run whose id equals the node's count is the current one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RunId(u64);

/// What writer sets hold: a node, or the lineage of a node created in a run (spec §7.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct WriterId(u64);

/// Where a creation sits in its creator's run, for its lineage (spec §7.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Place {
    /// The n-th creation at its site in the run.
    Nth(u64),
    /// A keyed creation, by its key's hash.
    Key(u64),
}

/// Lineage hashes carry the top bit, so they never equal a node's own id.
const LINEAGE: u64 = 1 << 63;

impl NodeId {
    /// The id's ordinal, for diagnostics.
    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for EpochId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for ScopeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Stamp {
    /// No change: older than every stamp the clock hands out.
    pub(crate) const NONE: Stamp = Stamp(0);

    /// Whether this is a stamp the clock handed out.
    pub(crate) fn is_some(self) -> bool {
        self.0 != 0
    }
}

impl RunId {
    /// The count before a node's first run.
    pub(crate) const NONE: RunId = RunId(0);
    /// An async source's block, registered at creation, runs under this id.
    pub(crate) const FIRST: RunId = RunId(1);
}

impl WriterId {
    /// A node writing under its own identity.
    pub(crate) fn node(id: NodeId) -> WriterId {
        WriterId(id.get())
    }

    /// A node created in a run, writing under its lineage hash.
    pub(crate) fn lineage(hash: u64) -> WriterId {
        WriterId(hash | LINEAGE)
    }
}

impl Bits for Stamp {
    fn to_bits(self) -> u64 {
        self.0
    }
    fn from_bits(bits: u64) -> Stamp {
        Stamp(bits)
    }
}

impl Bits for RunId {
    fn to_bits(self) -> u64 {
        self.0
    }
    fn from_bits(bits: u64) -> RunId {
        RunId(bits)
    }
}

/// An atomic cell holding a stamp or run id.
/// `let version: Atomic<Stamp> = Atomic::new(Stamp::NONE);`
pub(crate) struct Atomic<T: Bits>(crate::sync::AtomicU64, PhantomData<T>);

impl<T: Bits> Atomic<T> {
    pub(crate) fn new(value: T) -> Atomic<T> {
        Atomic(crate::sync::AtomicU64::new(value.to_bits()), PhantomData)
    }

    pub(crate) fn load(&self, order: Ordering) -> T {
        T::from_bits(self.0.load(order))
    }

    pub(crate) fn store(&self, value: T, order: Ordering) {
        self.0.store(value.to_bits(), order);
    }

    pub(crate) fn swap(&self, value: T, order: Ordering) -> T {
        T::from_bits(self.0.swap(value.to_bits(), order))
    }

    /// Raises the value to `value` if it is larger; returns the previous value.
    pub(crate) fn fetch_max(&self, value: T, order: Ordering) -> T {
        T::from_bits(self.0.fetch_max(value.to_bits(), order))
    }
}

impl Atomic<RunId> {
    /// Moves the count to the next run id and returns it.
    pub(crate) fn advance(&self, order: Ordering) -> RunId {
        RunId(self.0.fetch_add(1, order) + 1)
    }
}

// The clock and the two busiest counters each sit on lines of their own: every run reads the
// clock, and every publish, delivery and creation writes one of them.
static NEXT_NODE: Padded<std::sync::atomic::AtomicU64> = Padded(std::sync::atomic::AtomicU64::new(1));
static NEXT_EPOCH: Padded<std::sync::atomic::AtomicU64> = Padded(std::sync::atomic::AtomicU64::new(1));
static NEXT_SCOPE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
#[cfg(not(lithia_loom))]
static CLOCK: Padded<std::sync::atomic::AtomicU64> = Padded(std::sync::atomic::AtomicU64::new(1));
#[cfg(lithia_loom)]
loom::lazy_static! {
    static ref CLOCK: crate::sync::AtomicU64 = crate::sync::AtomicU64::new(1);
}

/// Allocates the next node id.
pub(crate) fn next_node_id() -> NodeId {
    let bits: u64 = NEXT_NODE.fetch_add(1, Ordering::SeqCst);
    NodeId(NonZeroU64::new(bits).expect("lithia: node ids start at 1"))
}

/// Allocates the next epoch id.
pub(crate) fn next_epoch_id() -> EpochId {
    EpochId(NEXT_EPOCH.fetch_add(1, Ordering::Relaxed))
}

/// Allocates the next scope id.
pub(crate) fn next_scope_id() -> ScopeId {
    ScopeId(NEXT_SCOPE.fetch_add(1, Ordering::Relaxed))
}

/// Takes a fresh stamp; callers publish their value first, then stamp it.
pub(crate) fn stamp() -> Stamp {
    Stamp(CLOCK.fetch_add(1, Ordering::SeqCst) + 1)
}

/// Reads the clock; a run reads it for `ran_at` before its body's first read.
pub(crate) fn now() -> Stamp {
    Stamp(CLOCK.load(Ordering::SeqCst))
}
