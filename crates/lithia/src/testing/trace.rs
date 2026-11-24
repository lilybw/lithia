// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! What `lithia::test::settled` follows beyond a node's subscribers: the nodes its runs created,
//! and those they wrote or reflowed without an edge (spec §11.3).

use std::sync::Arc;

use crate::graph::node::{NodeRef, WeakNodeRef};

/// A node's trace, kept under its scheduling lock and held weakly; allocated on first use, so a
/// node that never creates or writes without an edge carries one pointer.
pub(crate) struct Trace(Option<Box<Lists>>);

struct Lists {
    created: Vec<WeakNodeRef>,
    /// Length of `created` after its last prune; it is pruned again once twice as long.
    created_floor: usize,
    wrote: Vec<WeakNodeRef>,
}

impl Trace {
    pub(crate) const fn new() -> Trace {
        Trace(None)
    }

    /// The live nodes traced, for `settled` to follow.
    pub(crate) fn reached(&self) -> Vec<NodeRef> {
        match &self.0 {
            Some(lists) => lists.created.iter().chain(&lists.wrote).filter_map(WeakNodeRef::upgrade).collect(),
            None => Vec::new(),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.0 = None;
    }

    fn lists(&mut self) -> &mut Lists {
        self.0.get_or_insert_with(|| Box::new(Lists { created: Vec::new(), created_floor: 0, wrote: Vec::new() }))
    }
}

/// Traces a node created by a run; pruned once doubled since the last prune, so a run creating many
/// nodes stays linear.
pub(crate) fn note_created(trace: &mut Trace, node: &NodeRef) {
    let lists: &mut Lists = trace.lists();
    if lists.created.len() >= 2 * lists.created_floor.max(8) {
        lists.created.retain(|weak: &WeakNodeRef| weak.strong_count() > 0);
        lists.created_floor = lists.created.len();
    }
    lists.created.push(Arc::downgrade(node));
}

/// Traces a node a run wrote or reflowed without an edge, once.
pub(crate) fn note_wrote(trace: &mut Trace, node: &NodeRef) {
    let lists: &mut Lists = trace.lists();
    let target: *const () = Arc::as_ptr(node).cast::<()>();
    lists.wrote.retain(|weak: &WeakNodeRef| weak.strong_count() > 0);
    if !lists.wrote.iter().any(|weak: &WeakNodeRef| std::ptr::eq(weak.as_ptr().cast::<()>(), target)) {
        lists.wrote.push(Arc::downgrade(node));
    }
}
