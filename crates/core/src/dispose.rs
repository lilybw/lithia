//! Manual disposal and ghosts (spec §10.4, §10.5).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::context;
use crate::ids::NodeId;
use crate::node::{NodeRef, WeakNodeRef, phase};
use crate::protocol;

/// Frees `root` and every descendant with no live parent outside the freed branch.
pub(crate) fn dispose(root: &NodeRef) {
    context::assert_not_in_closure("lithia::dispose");
    if root.header().is_ghost() {
        return;
    }

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
        let header = node.header();
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
    for node in &freed {
        teardown(node);
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
}

fn is_ghost_parent(node: &NodeRef, parent: NodeId) -> bool {
    let parents = node.header().parents.lock();
    parents
        .as_ref()
        .and_then(|list: &Vec<NodeRef>| list.iter().find(|p: &&NodeRef| p.header().id == parent))
        .is_some_and(|p: &NodeRef| p.header().is_ghost())
}

/// Enters `Ghost` under the write lock, so no write can land afterwards.
fn mark_ghost(node: &NodeRef) {
    let header = node.header();
    let _write = header.write.lock();
    header.set_bits(phase::GHOST);
    header.clear_bits(phase::PENDING);
}

fn teardown(node: &NodeRef) {
    let header = node.header();
    protocol::retire(node);
    let parents: Option<Vec<NodeRef>> = header.parents.lock().take();
    if let Some(list) = parents.as_ref() {
        for parent in list {
            parent.header().unsubscribe(header.id);
        }
    }
    let sender: Option<tokio::sync::watch::Sender<u64>> = header.changes.lock().take();
    drop(sender);
    if header.counted.swap(false, Ordering::SeqCst) {
        header.scope.live.fetch_sub(1, Ordering::SeqCst);
    }
    node.release_payload();
    if let Some(list) = parents {
        crate::drops::release_parents(list);
    }
}
