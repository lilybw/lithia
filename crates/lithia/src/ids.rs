// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Process-wide identities and the version clock (spec §5.1, §11.4 step 5).

use std::sync::atomic::{AtomicU64, Ordering};

use crate::sync::Padded;

/// Process-unique node identity in creation order; creation order is topological.
pub(crate) type NodeId = u64;
/// Process-unique epoch identity.
pub(crate) type EpochId = u64;
/// A reading of the version clock; a later change carries a larger stamp, and 0 means none.
pub(crate) type Stamp = u64;
/// Counts a node's runs; the run whose id equals the node's count is the current one.
pub(crate) type RunId = u64;
/// Process-unique identity of a scope.
pub(crate) type ScopeId = u64;
/// What writer sets hold: a node's id, or a lineage hash with the top bit set (spec §7.4).
pub(crate) type WriterId = u64;

// The clock and the two busiest counters each sit on lines of their own: every run reads the
// clock, and every publish, delivery and creation writes one of them.
static NEXT_NODE: Padded<AtomicU64> = Padded(AtomicU64::new(1));
static NEXT_EPOCH: Padded<AtomicU64> = Padded(AtomicU64::new(1));
static NEXT_SCOPE: AtomicU64 = AtomicU64::new(1);
#[cfg(not(lithia_loom))]
static CLOCK: Padded<AtomicU64> = Padded(AtomicU64::new(1));
#[cfg(lithia_loom)]
loom::lazy_static! {
    static ref CLOCK: crate::sync::AtomicU64 = crate::sync::AtomicU64::new(1);
}

/// Allocates the next node id.
pub(crate) fn next_node_id() -> NodeId {
    NEXT_NODE.fetch_add(1, Ordering::SeqCst)
}

/// Allocates the next epoch id.
pub(crate) fn next_epoch_id() -> EpochId {
    NEXT_EPOCH.fetch_add(1, Ordering::Relaxed)
}

/// Allocates the next scope id.
pub(crate) fn next_scope_id() -> ScopeId {
    NEXT_SCOPE.fetch_add(1, Ordering::Relaxed)
}

/// Takes a fresh stamp; callers publish their value first, then stamp it.
pub(crate) fn stamp() -> Stamp {
    CLOCK.fetch_add(1, Ordering::SeqCst) + 1
}

/// Reads the clock; a run reads it for `ran_at` before its body's first read.
pub(crate) fn now() -> Stamp {
    CLOCK.load(Ordering::SeqCst)
}
