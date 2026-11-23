// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Manual disposal and ghosts (spec §10.4, §10.5).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::context;
use crate::graph::ids::NodeId;
use crate::graph::node::{ChangeSender, Header, NodeRef, Parents, WeakNodeRef, phase};
use crate::lifetime::teardown::detach;
use crate::protocol;
use crate::sync::MutexGuard;

/// Frees `root` and every descendant with no live parent outside the freed branch.
#[track_caller]
pub(crate) fn dispose(root: &NodeRef) {
    context::assert_not_in_closure("lithia::dispose");
    dispose_unchecked(root);
}

/// `dispose`, also from inside a closure: for a new node whose parents are all ghosts.
pub(crate) fn dispose_unchecked(root: &NodeRef) {
    if root.header().is_ghost() {
        return;
    }
    let _op: crate::lifetime::op::Operation = crate::lifetime::op::enter();

    // Every descendant, in NodeId order, which is topological.
    let mut closure: BTreeMap<NodeId, NodeRef> = BTreeMap::new();
    let mut stack: Vec<NodeRef> = vec![root.clone()];
    while let Some(node) = stack.pop() {
        for child in node.header().subscribers() {
            let id: NodeId = child.header().id;
            if let std::collections::btree_map::Entry::Vacant(slot) = closure.entry(id) {
                slot.insert(child.clone());
                stack.push(child);
            }
        }
    }

    let mut freed: Vec<NodeRef> = vec![root.clone()];
    let mut freed_ids: BTreeSet<NodeId> = BTreeSet::from([root.header().id]);
    let mut survivors: Vec<NodeRef> = Vec::new();
    for (id, node) in &closure {
        let header: &Header = node.header();
        if header.is_ghost() {
            continue;
        }
        let all_freed: bool = header
            .parent_ids
            .iter()
            .all(|parent: &NodeId| freed_ids.contains(parent) || is_ghost_parent(node, *parent));
        if all_freed {
            freed_ids.insert(*id);
            freed.push(node.clone());
        } else {
            survivors.push(node.clone());
        }
    }
    survivors.retain(|node: &NodeRef| !freed_ids.contains(&node.header().id));

    for node in &freed {
        mark_ghost(node);
    }
    // Children linked after the walk but before their parent closed are handled here, as the
    // walk would have; a child linked later finds the parent closed and handles itself.
    let mut late: Vec<NodeRef> = Vec::new();
    for node in &freed {
        for child in teardown(node) {
            let id: NodeId = child.header().id;
            if !closure.contains_key(&id) && !late.iter().any(|known: &NodeRef| known.header().id == id) {
                late.push(child);
            }
        }
    }
    let (orphans, late): (Vec<NodeRef>, Vec<NodeRef>) = late.into_iter().partition(|node: &NodeRef| {
        node.header()
            .parent_ids
            .iter()
            .all(|parent: &NodeId| freed_ids.contains(parent) || is_ghost_parent(node, *parent))
    });
    survivors.extend(late);
    // Support may be lost downstream: by the freed nodes' targets, generations and surviving children.
    for node in &freed {
        for target in node.header().targets.iter().filter_map(WeakNodeRef::upgrade) {
            crate::lifetime::support::raise(target);
        }
    }
    for node in &survivors {
        crate::lifetime::support::raise(node.clone());
    }
    let reruns: Vec<WeakNodeRef> = survivors
        .iter()
        .filter(|node: &&NodeRef| node.header().parent_ids.iter().any(|parent: &NodeId| freed_ids.contains(parent)))
        .map(Arc::downgrade)
        .collect();
    drop(closure);
    drop(survivors);
    context::record_reruns(reruns);
    drop(freed);
    for orphan in orphans {
        dispose_unchecked(&orphan);
    }
}

fn is_ghost_parent(node: &NodeRef, parent: NodeId) -> bool {
    let parents: MutexGuard<'_, Option<Parents>> = node.header().parents.lock();
    parents
        .as_ref()
        .and_then(|list: &Vec<NodeRef>| list.iter().find(|p: &&NodeRef| p.header().id == parent))
        .is_some_and(|p: &NodeRef| p.header().is_ghost())
}

/// Enters `Ghost` under the write lock, so no write can land afterwards.
fn mark_ghost(node: &NodeRef) {
    let header: &Header = node.header();
    let _write: MutexGuard<'_, ()> = header.write.lock();
    header.transition(phase::GHOST, phase::PENDING);
}

/// Tears a ghost down; returns the children linked when it closed.
fn teardown(node: &NodeRef) -> Vec<NodeRef> {
    let header: &Header = node.header();
    protocol::retire(node);
    // The children linked by the time it closes, under the lock `link` takes: a child linked later
    // finds the node closed.
    let (generation, linked): (Vec<NodeRef>, Vec<NodeRef>) = header.lock_sched().close();
    let me: Option<NodeRef> = header.lifetime.support.lock().take();
    if me.is_some() {
        crate::diag::support_released(header.site);
    }
    let sender: Option<ChangeSender> = header.changes.lock().take();
    drop(sender);
    node.release_payload();
    detach(header).release(generation);
    drop(me);
    linked
}
