// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Framing, releases and deciding an entry: run, settle, join a run, or cut (spec §7.1–§7.4).

use super::*;

/// Failures that make a fragile node fail in turn.
pub(super) type Causes = Vec<Arc<Failure>>;

/// A writer the writer check refused in `epoch`: it reruns once the epoch has settled, leaving
/// alone what it wrote on the chain (spec §7.4).
pub(super) struct CutRerun {
    pub(super) node: crate::graph::node::WeakNodeRef,
    pub(super) epoch: std::sync::Weak<Epoch>,
    pub(super) writers: WriterSet,
}

/// Opens an entry on every node downstream of `root` in `epoch`, through `children`, the subscribers
/// its obligation captured, so each wait counted here is one its parent's release will meet.
pub(crate) fn frame(root: NodeId, children: &[NodeRef], epoch: &Arc<Epoch>, work: &mut Work) {
    let mut stack: Vec<(NodeRef, Children)> = Vec::new();
    frame_level(root, children, epoch, &mut stack);
    while let Some((parent, children)) = stack.pop() {
        frame_level(parent.header().id, &children, epoch, &mut stack);
        work.keep.push(parent);
        work.keep.extend(children);
    }
}

/// Frames `children` of `parent_id` in `epoch`; those it opens an entry on go on `stack` with
/// their subscribers, unless a run of theirs still owes the epoch.
pub(super) fn frame_level(
    parent_id: NodeId,
    children: &[NodeRef],
    epoch: &Arc<Epoch>,
    stack: &mut Vec<(NodeRef, Children)>,
) {
    for child in children {
        let recurse: Option<Children> = {
            let header: &Header = child.header();
            let mut guard: SchedGuard<'_> = header.lock_sched();
            let sched: &mut Sched = &mut guard;
            match header.parent_index(parent_id) {
                None => None,
                Some(_) if header.is_ghost() => None,
                Some(index) => {
                    let (entry, framed): (&mut Entry, Option<Children>) = sched.open_entry(epoch);
                    entry.waiting.add(index);
                    entry.due.remove(Due::RUNNABLE);
                    framed
                }
            }
        };
        if let Some(grandchildren) = recurse {
            stack.push((child.clone(), grandchildren));
        }
    }
}

/// Reruns a cut writer in a child of the settled epoch, as a forced rerun on the chain without
/// itself, leaving alone what it wrote there; one cut again reruns once more, with more left alone.
pub(super) fn rerun_cut(rerun: CutRerun) {
    let (Some(node), Some(parent)) = (rerun.node.upgrade(), rerun.epoch.upgrade()) else {
        return;
    };
    let header: &Header = node.header();
    if header.is_ghost() {
        return;
    }
    let epoch: Arc<Epoch> = Epoch::child(&parent);
    header.lock_sched().suppress_in.push(parent.clone());
    let mut work: Work = Work::default();
    seed(&node, &epoch, &rerun.writers.without(Some(header.writer)), Seed::Rerun, &mut work);
    evaluate(&node, &mut work);
    work.keep.push(node);
    drop(parent);
    drain(work);
    epoch.close_token();
}

/// Releases each of `children` that waits on `from` in `epoch`, once.
pub(super) fn release_children(
    from: &NodeRef,
    children: Children,
    epoch: &Arc<Epoch>,
    carry: Stamp,
    writers: &WriterSet,
    origin: Option<&'static Site>,
    work: &mut Work,
) {
    let from_id: NodeId = from.header().id;
    for child in children {
        {
            let header: &Header = child.header();
            let mut guard: SchedGuard<'_> = header.lock_sched();
            let sched: &mut Sched = &mut guard;
            if let (Some(index), Some(entry)) = (header.parent_index(from_id), sched.entries.get_mut(&epoch.id))
                && entry.waiting.take(index)
            {
                entry.writers.union(writers);
                entry.newest = entry.newest.max(carry);
                if origin.is_some() {
                    entry.origin = origin;
                }
                if entry.writers.contains(header.writer) {
                    entry.due.insert(Due::CUT);
                }
                if entry.waiting.is_empty() {
                    decide(&child, sched, epoch.id, work);
                }
            }
        }
        work.keep(child);
    }
}

/// Failures of the parents that make a fragile node fail, if its condition holds (spec §9.2).
pub(super) fn fragility_causes(node: &NodeRef, fragility: Fragility) -> Option<Causes> {
    let header: &Header = node.header();
    let parents: MutexGuard<'_, Option<Parents>> = header.parents.lock();
    let list: &Parents = parents.as_ref()?;
    let mut causes: Causes = Vec::new();
    for parent in list {
        let parent_header: &Header = parent.header();
        let bits: u32 = parent_header.phase();
        if bits & phase::GHOST != 0 {
            if parent_header.shape.ghost == GhostPolicy::CauseFailure {
                causes.push(Arc::new(Failure::new("parent disposed", *parent_header.site)));
            }
        } else if bits & phase::FAILED != 0 {
            let failure: Arc<Failure> = parent_header
                .failure
                .load_full()
                .unwrap_or_else(|| Arc::new(Failure::new("failed", *parent_header.site)));
            causes.push(failure);
        }
    }
    let holds: bool = match fragility {
        Fragility::Fragile => !causes.is_empty(),
        Fragility::SemiFragile => !causes.is_empty() && causes.len() == list.len(),
    };
    holds.then_some(causes)
}

/// Whether a parent's version is newer than `ran_at`, the start of the node's last run.
pub(super) fn parents_changed_since(node: &NodeRef, ran_at: Stamp) -> bool {
    let parents: MutexGuard<'_, Option<Parents>> = node.header().parents.lock();
    parents.as_ref().is_some_and(|list: &Parents| {
        list.iter().any(|parent: &NodeRef| parent.header().version.load(Ordering::SeqCst) > ran_at)
    })
}

/// Epochs a `WHOLE_EPOCHS` node lets become ready while it waits before it runs on them anyway.
pub(crate) const WHOLE_EPOCHS_BOUND: u32 = 64;

/// Whether a `WHOLE_EPOCHS` node still waits for an epoch that has reached it to become ready.
pub(super) fn whole_epochs_wait(header: &Header, sched: &Sched) -> bool {
    header.shape.flags.contains(crate::Flags::WHOLE_EPOCHS)
        && sched.whole_deferred < WHOLE_EPOCHS_BOUND
        && sched.entries.values().any(|entry: &Entry| !entry.waiting.is_empty())
}

/// Fails a fragile node whose condition holds (spec §9.2).
pub(super) fn fail_by_fragility(header: &Header, sched: &mut Sched, causes: Causes, work: &mut Work) {
    let parent: String = causes.first().map(|cause: &Arc<Failure>| cause.message.clone()).unwrap_or_default();
    header.fail(Failure { causes, ..Failure::new("parent failed", *header.site) });
    sched.failed_by_fragility = true;
    work.diags.push(Diag::FragilityFailed { site: header.site, parent });
}

/// Decides what a ready entry does: run, wait for the run in flight, or settle (spec §7.2).
pub(super) fn decide(node: &NodeRef, sched: &mut Sched, epoch: EpochId, work: &mut Work) {
    let header: &Header = node.header();
    if header.is_ghost() {
        settle(node, sched, epoch, work);
        return;
    }
    if sched.unseeded {
        return;
    }
    let Some(entry) = sched.entries.get(&epoch) else {
        return;
    };
    if entry.due.contains(Due::CUT) {
        work.diags.push(Diag::Cut { writer: header.site, is_unit: header.is_unit, written: entry.origin });
        work.cut_reruns.push(CutRerun {
            node: Arc::downgrade(node),
            epoch: Arc::downgrade(&entry.epoch),
            writers: entry.writers.clone(),
        });
        settle(node, sched, epoch, work);
        return;
    }
    let mut force: bool = false;
    if let Some(fragility) = header.shape.fragility {
        match fragility_causes(node, fragility) {
            Some(causes) => {
                fail_by_fragility(header, sched, causes, work);
                // A run in flight would publish over the failure: it restarts, and the restart
                // finds the node fragile in `start_run`.
                if let Some(slot) = sched.run.as_mut()
                    && !slot.restart
                {
                    slot.supersede(header.site, work);
                    work.wakes.push(slot.shared.clone());
                }
                settle(node, sched, epoch, work);
                return;
            }
            // Recovered: a status change, which reruns the node only if it never ran or a parent
            // changed since its last run; otherwise it is restored as it was (spec §9.2).
            None if sched.failed_by_fragility => {
                if !sched.ran_at.is_some() || parents_changed_since(node, sched.ran_at) {
                    force = true;
                } else {
                    header.clear_bits(phase::FAILED);
                    header.failure.store(None);
                    sched.failed_by_fragility = false;
                }
            }
            None => {}
        }
    }
    let ran_at: Stamp = sched.ran_at;
    let Some(entry) = sched.entries.get_mut(&epoch) else {
        return;
    };
    entry.due.set(Due::FORCE, force || entry.due.contains(Due::FORCE));
    let changed: bool = entry.newest > ran_at;
    if !(entry.due.intersects(Due::FIRST | Due::FORCE | Due::REFLOW) || changed) {
        if entry.newest.is_some() {
            // A run started after this change absorbed it: the epoch waits for that run's item.
            if join_run(sched, epoch, work) {
                return;
            }
            // That run has released, or ended: the children test the node's current version.
            if let Some(entry) = sched.entries.get_mut(&epoch) {
                entry.carry = entry.carry.max(header.version.load(Ordering::SeqCst));
            }
        }
        settle(node, sched, epoch, work);
        return;
    }
    entry.due.insert(Due::RUNNABLE);
    let waits_for_run: bool = entry.reflow_only(ran_at);
    let mut whole_wait: bool = whole_epochs_wait(header, sched);
    if whole_wait && sched.run.is_none() {
        // Input that never pauses would keep it waiting: past the bound it runs on what is ready.
        sched.whole_deferred += 1;
        if sched.whole_deferred >= WHOLE_EPOCHS_BOUND {
            work.diags.push(Diag::WholeEpochsBound { site: header.site });
            whole_wait = false;
        }
    }
    match sched.run.as_mut() {
        // Under `WHOLE_EPOCHS`, the last epoch to become ready starts the run (or `settle` does).
        None if whole_wait => {}
        None => {
            sched.take_generation(&mut work.released);
            if !sched.queued {
                sched.queued = true;
                work.spawns.push(node.clone());
            }
        }
        Some(slot) => {
            // A run that protected another is not protected before its first item: the entry waits,
            // coalescing with any later ones, and supersedes it then, or runs after it ends.
            if !waits_for_run && !slot.restart && !slot.protected {
                slot.supersede(header.site, work);
                work.wakes.push(slot.shared.clone());
            }
        }
    }
}

/// Hands an entry over to the run in flight, if that run has not released yet: the run then releases
/// the entry's epoch with its first item, or when it ends (spec §7.2). True if it did.
pub(super) fn join_run(sched: &mut Sched, epoch: EpochId, work: &mut Work) -> bool {
    let unreleased: bool =
        sched.run.as_ref().is_some_and(|slot: &RunSlot| slot.served.iter().any(|served: &Served| !served.released));
    if !unreleased {
        return false;
    }
    let Some(entry) = sched.entries.remove(&epoch) else {
        return false;
    };
    if sched.entries.is_empty() {
        sched.whole_deferred = 0;
    }
    wake_gated(sched, work);
    let Entry { epoch, writers, carry, .. } = entry;
    if let Some(slot) = sched.run.as_mut() {
        merge_served(&mut slot.served, Served { epoch, writers, tokens: 1, released: false, carry });
    }
    true
}

/// Closes an entry without running; its children are released unless a run still owes them.
pub(super) fn settle(node: &NodeRef, sched: &mut Sched, epoch: EpochId, work: &mut Work) {
    let Some(entry) = sched.entries.remove(&epoch) else {
        return;
    };
    if sched.entries.is_empty() {
        sched.whole_deferred = 0;
    }
    wake_gated(sched, work);
    // The last epoch a `WHOLE_EPOCHS` node waited for settled: the run the others made due starts.
    if node.header().shape.flags.contains(crate::Flags::WHOLE_EPOCHS)
        && sched.run.is_none()
        && !sched.queued
        && !whole_epochs_wait(node.header(), sched)
        && sched.entries.values().any(|entry: &Entry| entry.due.contains(Due::RUNNABLE))
    {
        sched.take_generation(&mut work.released);
        sched.queued = true;
        work.spawns.push(node.clone());
    }
    match sched.owed_mut(epoch) {
        Some(served) => {
            served.carry = served.carry.max(entry.carry);
            served.writers.union(&entry.writers);
        }
        None => work.release(Release {
            from: node.clone(),
            children: sched.children(),
            epoch: entry.epoch.clone(),
            carry: entry.carry,
            writers: entry.writers,
            origin: entry.origin,
        }),
    }
    work.closes.push(entry.epoch);
}

/// Decides every ready entry not yet marked runnable.
pub(super) fn evaluate(node: &NodeRef, work: &mut Work) {
    let header: &Header = node.header();
    let mut guard: SchedGuard<'_> = header.lock_sched();
    let sched: &mut Sched = &mut guard;
    let ready: Vec<EpochId> = sched
        .entries
        .iter()
        .filter(|(_, entry): &(&EpochId, &Entry)| entry.waiting.is_empty() && !entry.due.contains(Due::RUNNABLE))
        .map(|(id, _): (&EpochId, &Entry)| *id)
        .collect();
    for epoch in ready {
        if sched.entries.contains_key(&epoch) {
            decide(node, sched, epoch, work);
        }
    }
}

/// How a node is seeded into an epoch.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Seed {
    /// A new node's first run.
    First,
    /// A surviving child of a disposed node reruns.
    Rerun,
}

pub(super) fn seed(node: &NodeRef, epoch: &Arc<Epoch>, writers: &WriterSet, kind: Seed, work: &mut Work) {
    let header: &Header = node.header();
    // The writer check: a node whose lineage already wrote on the chain does not run on it, so a
    // loop through creation is cut like any other (7.4).
    let cut: bool = writers.contains(header.writer);
    let created: Option<Children> = {
        let mut guard: SchedGuard<'_> = header.lock_sched();
        let sched: &mut Sched = &mut guard;
        if header.is_ghost() {
            return;
        }
        if kind == Seed::First {
            sched.unseeded = false;
        }
        let (entry, framed): (&mut Entry, Option<Children>) = sched.open_entry(epoch);
        entry.due.insert(match kind {
            Seed::First => Due::FIRST,
            Seed::Rerun => Due::FORCE,
        });
        entry.writers.union(writers);
        if cut {
            entry.due.insert(Due::CUT);
        }
        framed
    };
    if let Some(children) = created {
        frame(header.id, &children, epoch, work);
        work.keep.extend(children);
    }
}

pub(super) fn seed_reflow(node: &NodeRef, epoch: &Arc<Epoch>, chain: &WriterSet, stamp: Stamp, work: &mut Work) {
    let header: &Header = node.header();
    let created: Option<Children> = {
        let mut guard: SchedGuard<'_> = header.lock_sched();
        let sched: &mut Sched = &mut guard;
        if header.is_ghost() {
            return;
        }
        let cut: bool = chain.contains(header.writer);
        let (entry, framed): (&mut Entry, Option<Children>) = sched.open_entry(epoch);
        entry.due.insert(Due::REFLOW);
        entry.carry = entry.carry.max(stamp);
        entry.writers.union(chain);
        if cut {
            entry.due.insert(Due::CUT);
        }
        framed
    };
    if let Some(children) = created {
        frame(header.id, &children, epoch, work);
        work.keep.extend(children);
    }
}
