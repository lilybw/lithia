// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! What `lithia::test::settled` follows beyond a node's subscribers: the nodes its runs created,
//! and those they wrote or reflowed without an edge (spec §11.3).

use std::sync::Arc;

use crate::graph::node::{NodeRef, WeakNodeRef};

/// A node's trace, kept under its scheduling lock and held weakly.
pub(crate) struct Trace {
    created: Vec<WeakNodeRef>,
    /// Length of `created` after its last prune; it is pruned again once twice as long.
    created_floor: usize,
    wrote: Vec<WeakNodeRef>,
}

impl Trace {
    pub(crate) const fn new() -> Trace {
        Trace { created: Vec::new(), created_floor: 0, wrote: Vec::new() }
    }

    /// The live nodes traced, for `settled` to follow.
    pub(crate) fn reached(&self) -> Vec<NodeRef> {
        self.created.iter().chain(&self.wrote).filter_map(WeakNodeRef::upgrade).collect()
    }

    pub(crate) fn clear(&mut self) {
        self.created.clear();
        self.wrote.clear();
    }
}

/// Traces a node created by a run; pruned once doubled since the last prune, so a run creating many
/// nodes stays linear.
pub(crate) fn note_created(trace: &mut Trace, node: &NodeRef) {
    if trace.created.len() >= 2 * trace.created_floor.max(8) {
        trace.created.retain(|weak: &WeakNodeRef| weak.strong_count() > 0);
        trace.created_floor = trace.created.len();
    }
    trace.created.push(Arc::downgrade(node));
}

/// Traces a node a run wrote or reflowed without an edge, once.
pub(crate) fn note_wrote(trace: &mut Trace, node: &NodeRef) {
    let target: *const () = Arc::as_ptr(node).cast::<()>();
    trace.wrote.retain(|weak: &WeakNodeRef| weak.strong_count() > 0);
    if !trace.wrote.iter().any(|weak: &WeakNodeRef| std::ptr::eq(weak.as_ptr().cast::<()>(), target)) {
        trace.wrote.push(Arc::downgrade(node));
    }
}
