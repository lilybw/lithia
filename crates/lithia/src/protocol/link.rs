// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Linking a new node to its parents and opening the entries they owe it (spec §7.1, §10.1).

use super::*;

/// Links a new node to its parents and opens entries for every epoch a parent still owes
/// a release in (spec §11.4 step 7). Returns whether every parent had been torn down as a ghost.
pub(crate) fn link(node: &NodeRef, parents: &[NodeRef]) -> bool {
    let header: &Header = node.header();
    let weak: crate::graph::node::WeakNodeRef = Arc::downgrade(node);
    let mut all_closed: bool = true;
    let mut guard: SchedGuard<'_> = header.lock_sched();
    for parent in parents {
        let parent_header: &Header = parent.header();
        let epochs: Epochs = {
            let mut parent_guard: SchedGuard<'_> = parent_header.lock_sched();
            let parent_sched: &mut Sched = &mut parent_guard;
            all_closed &= parent_sched.closed;
            parent_sched.subscribers.insert(header.id, weak.clone());
            let mut list: Epochs = parent_sched.entries.values().map(|entry: &Entry| entry.epoch.clone()).collect();
            if let Some(slot) = &parent_sched.run {
                list.extend(slot.served.iter().filter(|s: &&Served| !s.released).map(|s: &Served| s.epoch.clone()));
            }
            list.extend(
                parent_sched.handover.iter().filter(|s: &&Served| !s.released).map(|s: &Served| s.epoch.clone()),
            );
            list
        };
        let Some(index) = header.parent_index(parent_header.id) else {
            continue;
        };
        for epoch in epochs {
            let entry: &mut Entry = guard.entries.get_or_insert_with(epoch.id, || {
                epoch.open_token();
                Entry::new(epoch.clone())
            });
            // One release per epoch: an entry and a run owing the epoch release once between them.
            entry.waiting.ensure(index);
        }
    }
    drop(guard);
    progress::bump();
    all_closed
}

/// How a created node stands with its creator when a run of its would start.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Standing {
    /// No creator holds it: it was made outside a run, or let go of.
    Free,
    /// Its creator has decided it.
    Kept,
    /// Its creator may still replace or keep it: it waits, registered with the creator.
    Gated,
}

/// How `node` stands with `creator`; a wait is registered when gated. Locks the creator inside the
/// node's lock: creators are older, as parents are in `link`. A release landing afterwards resumes it.
pub(super) fn standing(node: &NodeRef, creator: Option<&NodeRef>) -> Standing {
    let Some(creator) = creator else {
        return Standing::Free;
    };
    let header: &Header = node.header();
    let mut guard: SchedGuard<'_> = creator.header().lock_sched();
    if header.lifetime.held_by.lock().is_none() {
        return Standing::Free;
    }
    if header.is_ghost() || !guard.decides(header) {
        return Standing::Kept;
    }
    if !guard.gated.iter().any(|(id, _): &(NodeId, crate::graph::node::WeakNodeRef)| *id == header.id) {
        guard.gated.push((header.id, Arc::downgrade(node)));
    }
    Standing::Gated
}
