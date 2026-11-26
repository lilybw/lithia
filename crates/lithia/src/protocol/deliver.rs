// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Delivering a batch's writes, creations and reflows into the epochs it belongs to (spec §6.2, §7.1).

use super::*;

/// Epochs a run serves, each with the writer set its entry carried.
pub(crate) type ServedEpochs = Vec<(Arc<Epoch>, WriterSet)>;

/// Epochs a batch is delivered into, each with the writer set its chains start from.
pub(super) type Targets = Vec<(Arc<Epoch>, WriterSet)>;

/// A node's own item delivered in a later poll: its children run in the child epoch.
pub(crate) struct OwnItem<'a> {
    pub(crate) node: &'a NodeRef,
    pub(crate) stamp: Stamp,
}

/// Live nodes of `list` by id, once each, with the newest stamp recorded for each.
pub(super) fn upgrade_sorted(mut list: Vec<(crate::graph::node::WeakNodeRef, NodeId, Stamp)>) -> Vec<(NodeRef, Stamp)> {
    list.sort_by_key(|(_, id, _): &(crate::graph::node::WeakNodeRef, NodeId, Stamp)| *id);
    let mut out: Vec<(NodeRef, Stamp)> = Vec::with_capacity(list.len());
    for (weak, id, stamp) in list {
        match out.last_mut() {
            Some((last, newest)) if last.header().id == id => *newest = (*newest).max(stamp),
            _ => {
                if let Some(node) = weak.upgrade() {
                    out.push((node, stamp));
                }
            }
        }
    }
    out
}

pub(super) fn upgrade_nodes(list: Vec<crate::graph::node::WeakNodeRef>) -> Vec<NodeRef> {
    let mut out: Vec<NodeRef> =
        list.into_iter().filter_map(|weak: crate::graph::node::WeakNodeRef| weak.upgrade()).collect();
    out.sort_by_key(|node: &NodeRef| node.header().id);
    out.dedup_by_key(|node: &mut NodeRef| node.header().id);
    out
}

/// Delivers a batch into `targets`, each an epoch with its base writer set (spec §7.1, §7.4);
/// `writer`'s writes route against the base set plus itself, its creations seed with it removed.
pub(crate) fn deliver_into(
    batch: Batch,
    targets: &[(Arc<Epoch>, WriterSet)],
    writer: Option<WriterId>,
    own: Option<OwnItem<'_>>,
) {
    if batch.is_empty() && own.is_none() {
        return;
    }
    let Batch { writes, reflows, creations, reruns, refreshes, defers } = batch;
    let writes: Vec<(NodeRef, Stamp)> = upgrade_sorted(writes);
    let reflows: Vec<(NodeRef, Stamp)> = upgrade_sorted(reflows);
    let creations: Vec<NodeRef> = upgrade_nodes(creations);
    let reruns: Vec<NodeRef> = upgrade_nodes(reruns);
    let refreshes: Vec<NodeRef> = upgrade_nodes(refreshes);
    let mut work: Work = Work::default();
    if let Some(writer) = writer {
        for (epoch, _) in targets {
            for (node, _) in writes.iter().chain(reflows.iter()) {
                epoch.log_write(writer, node.header().id);
            }
        }
    }

    // Framing: the whole downstream region opens its entries before anything is released. Each
    // written node's release reaches the subscribers its framing counted, in the same order.
    let mut counted: Vec<Children> = Vec::new();
    for (epoch, base) in targets {
        let chain: WriterSet = base.with(writer);
        let seed_writers: WriterSet = base.without(writer);
        for node in &creations {
            seed(node, epoch, &seed_writers, Seed::First, &mut work);
        }
        for node in &reruns {
            // Disposal is no write: survivors rerun unattributed, so nothing can cut them.
            seed(node, epoch, &WriterSet::new(), Seed::Rerun, &mut work);
        }
        for node in &refreshes {
            // A reclaimed node reruns on new captures, seeded like a creation.
            seed(node, epoch, &seed_writers, Seed::Rerun, &mut work);
        }
        for (node, _) in &writes {
            let children: Children = node.header().subscribers();
            frame(node.header(), &children, epoch, &mut work);
            counted.push(children);
        }
        for (node, stamp) in &reflows {
            if node.header().kind == Kind::Derived {
                seed_reflow(node, epoch, &chain, *stamp, &mut work);
            } else {
                let children: Children = node.header().subscribers();
                frame(node.header(), &children, epoch, &mut work);
                counted.push(children);
            }
        }
        if let Some(item) = &own {
            let children: Children = item.node.header().subscribers();
            frame(item.node.header(), &children, epoch, &mut work);
            counted.push(children);
        }
    }

    // Releases.
    let mut counted: std::vec::IntoIter<Children> = counted.into_iter();
    for (epoch, base) in targets {
        let chain: WriterSet = base.with(writer);
        for (node, stamp) in &writes {
            let children: Children = counted.next().unwrap_or_default();
            release_children(node, children, epoch, *stamp, &chain, Some(node.header().site), &mut work);
        }
        for (node, stamp) in &reflows {
            if node.header().kind != Kind::Derived {
                let children: Children = counted.next().unwrap_or_default();
                release_children(node, children, epoch, *stamp, &chain, Some(node.header().site), &mut work);
            }
        }
        if let Some(item) = &own {
            let children: Children = counted.next().unwrap_or_default();
            release_children(item.node, children, epoch, item.stamp, base, None, &mut work);
        }
    }
    for node in creations.iter().chain(reruns.iter()).chain(refreshes.iter()) {
        evaluate(node, &mut work);
    }
    for (node, _) in &reflows {
        if node.header().kind == Kind::Derived {
            evaluate(node, &mut work);
        }
    }
    if !defers.is_empty() {
        let epochs: Epochs = targets.iter().map(|(epoch, _): &(Arc<Epoch>, WriterSet)| epoch.clone()).collect();
        attach_defers(epochs, defers);
    }
    work.keep.extend(writes.into_iter().map(|(node, _)| node));
    work.keep.extend(reflows.into_iter().map(|(node, _)| node));
    work.keep.extend(creations);
    work.keep.extend(reruns);
    work.keep.extend(refreshes);
    drain(work);
}

/// Delivers an outside batch as a new root epoch of `scope`, with an empty writer set.
pub(crate) fn deliver_outside(batch: Batch, scope: &Arc<Scope>) {
    deliver_outside_deferred(batch, scope).close();
}

/// Like `deliver_outside`, but hands the epoch's guard token back to the caller.
pub(crate) fn deliver_outside_deferred(batch: Batch, scope: &Arc<Scope>) -> Tokens {
    let mut tokens: Tokens = Tokens::new();
    if batch.is_empty() {
        return tokens;
    }
    let epoch: Arc<Epoch> = Epoch::root(scope);
    scope.set_last(&epoch);
    deliver_into(batch, &[(epoch.clone(), WriterSet::new())], None, None);
    tokens.push(epoch);
    tokens
}

/// Delivers a run's later batch as one child epoch per epoch it serves, with the node's item if it
/// has a stamp or the node failed; returns them still holding their guard token, which the caller closes.
pub(crate) fn deliver_children(
    node: &NodeRef,
    served: &[(Arc<Epoch>, WriterSet)],
    batch: Batch,
    own_stamp: Stamp,
    failed: bool,
) -> Tokens {
    let mut tokens: Tokens = Tokens::new();
    if batch.is_empty() && !own_stamp.is_some() && !failed {
        return tokens;
    }
    let children: Targets = served
        .iter()
        .map(|(epoch, writers): &(Arc<Epoch>, WriterSet)| (Epoch::child(epoch), writers.clone()))
        .collect();
    // A failure reaches the children as an item that changed nothing: each checks its resilience.
    let own: Option<OwnItem<'_>> = (own_stamp.is_some() || failed).then_some(OwnItem { node, stamp: own_stamp });
    deliver_into(batch, &children, Some(node.header().writer), own);
    for (epoch, _) in children {
        tokens.push(epoch);
    }
    tokens
}

/// Takes the current frame's pending batch and target; outside lithia tasks, there is neither.
pub(crate) fn take_pending() -> Option<(Batch, Target)> {
    context::with_frame(|frame: Option<&Frame>| frame.map(|frame: &Frame| (frame.take_batch(), frame.target.clone())))
}

/// Delivers `batch` for `target`, returning the child epochs it opened for a run in flight.
pub(crate) fn deliver(target: &Target, batch: Batch) -> Epochs {
    match target {
        Target::Scope(scope) => {
            deliver_outside(batch, scope);
            Epochs::new()
        }
        Target::Run(run) => deliver_for_run(run, batch),
    }
}

/// The epochs a run serves, with their writer sets, if `run` is still in flight.
fn served_by(run: &Arc<RunControl>) -> Option<(NodeRef, ServedEpochs)> {
    let node: NodeRef = run.node.upgrade()?;
    let served: Option<ServedEpochs> = {
        let guard: SchedGuard<'_> = node.header().lock_sched();
        match &guard.run {
            Some(slot) if slot.id == run.id() && !run.is_cancelled() => {
                Some(slot.served.iter().map(|s: &Served| (s.epoch.clone(), s.writers.clone())).collect())
            }
            _ => None,
        }
    };
    served.map(|served: ServedEpochs| (node, served))
}

/// Delivers a run's batch as child epochs while it is in flight, else as an outside batch.
fn deliver_for_run(run: &Arc<RunControl>, batch: Batch) -> Epochs {
    match served_by(run) {
        Some((node, served)) => {
            let tokens: Tokens = deliver_children(&node, &served, batch, Stamp::NONE, false);
            drop(node);
            let epochs: Epochs = tokens.epochs();
            tokens.close();
            epochs
        }
        None => {
            deliver_outside(batch, &run.scope());
            Epochs::new()
        }
    }
}
