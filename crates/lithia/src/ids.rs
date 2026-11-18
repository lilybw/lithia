// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Process-wide identities and the version clock (spec §5.1, §11.4 step 5).

use std::sync::atomic::{AtomicU64, Ordering};

/// Process-unique node identity in creation order; creation order is topological.
pub(crate) type NodeId = u64;
/// Process-unique epoch identity.
pub(crate) type EpochId = u64;

static NEXT_NODE: AtomicU64 = AtomicU64::new(1);
static NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);
static NEXT_SCOPE: AtomicU64 = AtomicU64::new(1);
#[cfg(not(lithia_loom))]
static CLOCK: AtomicU64 = AtomicU64::new(1);
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
pub(crate) fn next_scope_id() -> u64 {
    NEXT_SCOPE.fetch_add(1, Ordering::Relaxed)
}

/// Takes a fresh stamp; callers publish their value first, then stamp it.
pub(crate) fn stamp() -> u64 {
    CLOCK.fetch_add(1, Ordering::SeqCst) + 1
}

/// Reads the clock; a run reads it for `ran_at` before its body's first read.
pub(crate) fn now() -> u64 {
    CLOCK.load(Ordering::SeqCst)
}
