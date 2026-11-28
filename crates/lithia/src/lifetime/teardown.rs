// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Detaching a node that is freed or torn down: the steps both paths share (spec §10.1, §10.4).

use std::sync::atomic::Ordering;

use crate::expand::capture::SharedLease;
use crate::graph::node::{Header, NodeRef, Parents};

/// What a detached node still holds, released once nothing it held depends on the order.
#[must_use = "a detached node's generation and parents are released with `Detached::release`"]
pub(crate) struct Detached {
    parents: Option<Parents>,
}

/// Takes the node off its scope's live count and its parents' subscriber lists, and ends the lease
/// of the block that moved handles out, so they count before support is recomputed.
pub(crate) fn detach(header: &Header) -> Detached {
    if header.lifetime.counted.swap(false, Ordering::SeqCst) {
        let before: usize = header.scope.live.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(before > 0, "lithia: scope {} counted fewer live nodes than it holds", header.scope.id);
    }
    let parents: Option<Parents> = header.parents.lock().take();
    if let Some(list) = parents.as_ref() {
        for parent in list {
            parent.header().unsubscribe(header.id);
        }
    }
    let lease: Option<SharedLease> = header.lifetime.lease.lock().take();
    if let Some(lease) = lease {
        lease.end();
    }
    Detached { parents }
}

impl Detached {
    /// Releases the node's generation, then its parents, iteratively so long chains cannot overflow.
    pub(crate) fn release(self, generation: Vec<NodeRef>) {
        crate::lifetime::support::release_generation(generation);
        if let Some(list) = self.parents {
            crate::lifetime::drops::release_parents(list);
        }
    }
}
