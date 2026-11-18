// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! The scheduling protocol: framing, releases, runs and settling (spec §7, §11.4).
//! Its locking and release invariants are stated in spec §11.4.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::sync::{AtomicUsize, Mutex, MutexGuard};

use crate::context::{Batch, Deferred};
use crate::diag::Diag;
use crate::epoch::{Epoch, Scope};
use crate::ids::{self, EpochId, NodeId};
use crate::node::{Entry, Header, Kind, NodeRef, RunSlot, Sched, Served, merge_served, phase};
use crate::options::{Fragility, GhostPolicy};
use crate::run::{RunShared, TaskFuture};
use crate::sets::WriterSet;
use crate::{Failure, Site, diag, settle_wait};

/// Epochs a run serves, each with the writer set its entry carried.
pub(crate) type ServedEpochs = Vec<(Arc<Epoch>, WriterSet)>;

/// A parent telling its waiting children that it settled an epoch.
pub(crate) struct Release {
    from: NodeRef,
    epoch: Arc<Epoch>,
    /// Change stamp to compare against each child's last run, or 0.
    carry: u64,
    writers: WriterSet,
    origin: Option<&'static Site>,
}

/// Actions gathered under locks and carried out after unlocking.
#[derive(Default)]
pub(crate) struct Work {
    releases: VecDeque<Release>,
    spawns: Vec<NodeRef>,
    wakes: Vec<Arc<RunShared>>,
    closes: Vec<Arc<Epoch>>,
    diags: Vec<Diag>,
    /// Strong references taken from weak lists, dropped only after every lock is released.
    keep: Vec<NodeRef>,
    /// Generations released by this step, let go of before its spawns.
    released: Vec<NodeRef>,
    /// Created nodes their creator gated, to start again once it has moved on.
    resumes: Vec<crate::node::WeakNodeRef>,
}

/// Carries out gathered work: releases, spawns and wakes, then token closes.
pub(crate) fn drain(work: Work) {
    let closes: Vec<Arc<Epoch>> = drain_deferred(work);
    close_all(closes);
}

/// Closes tokens; callers drop every strong node reference first, so a settled scope
/// never finds a node kept alive only by lithia (spec §10.1).
pub(crate) fn close_all(epochs: Vec<Arc<Epoch>>) {
    for epoch in epochs {
        epoch.close_token();
    }
    settle_wait::bump();
}

/// Like `drain`, but hands the token closes back to the caller.
#[must_use = "the returned tokens must be closed with close_all"]
pub(crate) fn drain_deferred(mut work: Work) -> Vec<Arc<Epoch>> {
    while let Some(release) = work.releases.pop_front() {
        release_children(&release.from, &release.epoch, release.carry, &release.writers, release.origin, &mut work);
        work.keep.push(release.from);
    }
    for diag in std::mem::take(&mut work.diags) {
        diag.emit();
    }
    {
        // The step's other references and its released generations go before its spawns,
        // so an idle old child released here is freed before its task exists.
        let _op: crate::support::OpGuard = crate::support::enter();
        drop(std::mem::take(&mut work.keep));
        crate::support::release_generation(std::mem::take(&mut work.released));
    }
    let mut spawns: Vec<NodeRef> = std::mem::take(&mut work.spawns);
    for weak in std::mem::take(&mut work.resumes) {
        if let Some(node) = weak.upgrade()
            && queue_if_runnable(&node)
        {
            spawns.push(node);
        }
    }
    spawns.sort_by_key(|node: &NodeRef| node.header().id);
    spawns.dedup_by_key(|node: &mut NodeRef| node.header().id);
    for node in spawns {
        node.spawn_run();
    }
    for run in std::mem::take(&mut work.wakes) {
        run.wake();
    }
    let closes: Vec<Arc<Epoch>> = std::mem::take(&mut work.closes);
    drop(work);
    settle_wait::bump();
    closes
}

/// Queues a run of an idle node that has work left, as one its creator gated; true if it did.
fn queue_if_runnable(node: &NodeRef) -> bool {
    let mut guard: MutexGuard<'_, Sched> = node.header().sched.lock();
    let ready: bool = !guard.inherited.is_empty()
        || guard.entries.values().any(|entry: &Entry| entry.runnable && entry.waiting.is_empty());
    let queue: bool = ready && guard.run.is_none() && !guard.queued && !node.header().is_ghost();
    if queue {
        guard.queued = true;
    }
    queue
}

/// Starts a gated node again, if it has work left (spec §7.2, Creator order).
pub(crate) fn resume(node: &NodeRef) {
    let mut work: Work = Work::default();
    work.resumes.push(Arc::downgrade(node));
    drain(work);
}

/// Hands the nodes this creator gated to `work`, to start again; they gate again if it still decides them.
fn wake_gated(sched: &mut Sched, work: &mut Work) {
    work.resumes.extend(sched.gated.drain(..).map(|(_, weak): (NodeId, crate::node::WeakNodeRef)| weak));
}

/// Opens an entry on every node downstream of `root` in `epoch`, waiting on its parent.
pub(crate) fn frame(root: &NodeRef, epoch: &Arc<Epoch>, work: &mut Work) {
    let mut stack: Vec<NodeRef> = vec![root.clone()];
    while let Some(parent) = stack.pop() {
        let parent_id: NodeId = parent.header().id;
        for child in parent.header().subscribers() {
            let recurse: bool = {
                let header: &Header = child.header();
                let mut guard: MutexGuard<'_, Sched> = header.sched.lock();
                let sched: &mut Sched = &mut guard;
                match header.parent_index(parent_id) {
                    None => false,
                    Some(_) if header.is_ghost() => false,
                    Some(index) => {
                        if let Some(entry) = sched.entries.get_mut(&epoch.id) {
                            entry.waiting.set(index);
                            entry.runnable = false;
                            false
                        } else {
                            let owes: bool = sched.owes(epoch.id);
                            epoch.open_token();
                            let mut entry: Entry = Entry::new(epoch.clone());
                            entry.waiting.set(index);
                            sched.entries.insert(epoch.id, entry);
                            !owes
                        }
                    }
                }
            };
            if recurse {
                stack.push(child.clone());
            }
            work.keep.push(child);
        }
        work.keep.push(parent);
    }
}

/// Releases every child of `from` that waits on it in `epoch`.
fn release_children(
    from: &NodeRef,
    epoch: &Arc<Epoch>,
    carry: u64,
    writers: &WriterSet,
    origin: Option<&'static Site>,
    work: &mut Work,
) {
    let from_id: NodeId = from.header().id;
    for child in from.header().subscribers() {
        {
            let header: &Header = child.header();
            let mut guard: MutexGuard<'_, Sched> = header.sched.lock();
            let sched: &mut Sched = &mut guard;
            if let (Some(index), Some(entry)) = (header.parent_index(from_id), sched.entries.get_mut(&epoch.id))
                && entry.waiting.get(index)
            {
                entry.waiting.clear(index);
                entry.writers.union(writers);
                entry.newest = entry.newest.max(carry);
                if origin.is_some() {
                    entry.origin = origin;
                }
                if entry.writers.contains(header.id) {
                    entry.cut = true;
                }
                if entry.waiting.is_empty() {
                    decide(&child, sched, epoch.id, work);
                }
            }
        }
        work.keep.push(child);
    }
}

/// Failures of the parents that make a fragile node fail, if its condition holds (spec §9.2).
fn fragility_causes(node: &NodeRef, fragility: Fragility) -> Option<Vec<Arc<Failure>>> {
    let header: &Header = node.header();
    let parents: MutexGuard<'_, Option<Vec<NodeRef>>> = header.parents.lock();
    let list: &Vec<NodeRef> = parents.as_ref()?;
    let mut causes: Vec<Arc<Failure>> = Vec::new();
    for parent in list {
        let parent_header: &Header = parent.header();
        let bits: u32 = parent_header.phase();
        if bits & phase::GHOST != 0 {
            if parent_header.ghost == GhostPolicy::CauseFailure {
                causes.push(Arc::new(Failure {
                    message: "parent disposed".to_string(),
                    site: *parent_header.site,
                    causes: Vec::new(),
                }));
            }
        } else if bits & phase::FAILED != 0 {
            let failure: Arc<Failure> = parent_header.failure.load_full().unwrap_or_else(|| {
                Arc::new(Failure { message: "failed".to_string(), site: *parent_header.site, causes: Vec::new() })
            });
            causes.push(failure);
        }
    }
    let holds: bool = match fragility {
        Fragility::Fragile => !causes.is_empty(),
        Fragility::SemiFragile => !causes.is_empty() && causes.len() == list.len(),
    };
    holds.then_some(causes)
}

/// Fails a fragile node whose condition holds (spec §9.2).
fn fail_by_fragility(header: &Header, sched: &mut Sched, causes: Vec<Arc<Failure>>, work: &mut Work) {
    let parent: String = causes.first().map(|cause: &Arc<Failure>| cause.message.clone()).unwrap_or_default();
    header.fail(Failure { message: "parent failed".to_string(), site: *header.site, causes });
    sched.failed_by_fragility = true;
    work.diags.push(Diag::FragilityFailed { site: header.site, parent });
}

/// Decides what a ready entry does: run, wait for the run in flight, or settle (spec §7.2).
fn decide(node: &NodeRef, sched: &mut Sched, epoch: EpochId, work: &mut Work) {
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
    if entry.cut {
        work.diags.push(Diag::Cut { writer: header.site, is_unit: header.is_unit, written: entry.origin });
        settle(node, sched, epoch, work);
        return;
    }
    let mut force: bool = false;
    if let Some(fragility) = header.fragility {
        match fragility_causes(node, fragility) {
            Some(causes) => {
                fail_by_fragility(header, sched, causes, work);
                // A run in flight would publish over the failure: it restarts, and the restart
                // finds the node fragile in `start_run`.
                if let Some(slot) = sched.run.as_mut()
                    && !slot.restart
                {
                    slot.restart = true;
                    slot.shared.cancel();
                    work.diags.push(Diag::Superseded { site: header.site, run: slot.id });
                    work.wakes.push(slot.shared.clone());
                }
                settle(node, sched, epoch, work);
                return;
            }
            None => force = sched.failed_by_fragility,
        }
    }
    let ran_at: u64 = sched.ran_at;
    let Some(entry) = sched.entries.get_mut(&epoch) else {
        return;
    };
    entry.force |= force;
    let changed: bool = entry.newest > ran_at;
    if !(entry.first || entry.force || entry.reflow || changed) {
        settle(node, sched, epoch, work);
        return;
    }
    entry.runnable = true;
    let waits_for_run: bool = entry.reflow && !entry.first && !entry.force && !changed;
    match sched.run.as_mut() {
        None => {
            sched.release_generation(&mut work.released);
            if !sched.queued {
                sched.queued = true;
                work.spawns.push(node.clone());
            }
        }
        Some(slot) => {
            if !waits_for_run && !slot.restart {
                slot.restart = true;
                slot.shared.cancel();
                work.diags.push(Diag::Superseded { site: header.site, run: slot.id });
                work.wakes.push(slot.shared.clone());
            }
        }
    }
}

/// Closes an entry without running; its children are released unless a run still owes them.
fn settle(node: &NodeRef, sched: &mut Sched, epoch: EpochId, work: &mut Work) {
    let Some(entry) = sched.entries.remove(&epoch) else {
        return;
    };
    wake_gated(sched, work);
    match sched.owed_mut(epoch) {
        Some(served) => {
            served.carry = served.carry.max(entry.carry);
            served.writers.union(&entry.writers);
        }
        None => work.releases.push_back(Release {
            from: node.clone(),
            epoch: entry.epoch.clone(),
            carry: entry.carry,
            writers: entry.writers,
            origin: entry.origin,
        }),
    }
    work.closes.push(entry.epoch);
}

/// Decides every ready entry not yet marked runnable.
fn evaluate(node: &NodeRef, work: &mut Work) {
    let header: &Header = node.header();
    let mut guard: MutexGuard<'_, Sched> = header.sched.lock();
    let sched: &mut Sched = &mut guard;
    let ready: Vec<EpochId> = sched
        .entries
        .iter()
        .filter(|(_, entry): &(&EpochId, &Entry)| entry.waiting.is_empty() && !entry.runnable)
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
enum Seed {
    /// A new node's first run.
    First,
    /// A surviving child of a disposed node reruns.
    Rerun,
}

fn seed(node: &NodeRef, epoch: &Arc<Epoch>, writers: &WriterSet, kind: Seed, work: &mut Work) {
    let header: &Header = node.header();
    let created: bool = {
        let mut guard: MutexGuard<'_, Sched> = header.sched.lock();
        let sched: &mut Sched = &mut guard;
        if header.is_ghost() {
            return;
        }
        if kind == Seed::First {
            sched.unseeded = false;
        }
        match sched.entries.get_mut(&epoch.id) {
            Some(entry) => {
                match kind {
                    Seed::First => entry.first = true,
                    Seed::Rerun => entry.force = true,
                }
                entry.writers.union(writers);
                false
            }
            None => {
                let owes: bool = sched.owes(epoch.id);
                epoch.open_token();
                let mut entry: Entry = Entry::new(epoch.clone());
                match kind {
                    Seed::First => entry.first = true,
                    Seed::Rerun => entry.force = true,
                }
                entry.writers = writers.clone();
                sched.entries.insert(epoch.id, entry);
                !owes
            }
        }
    };
    if created {
        frame(node, epoch, work);
    }
}

fn seed_reflow(node: &NodeRef, epoch: &Arc<Epoch>, chain: &WriterSet, stamp: u64, work: &mut Work) {
    let header: &Header = node.header();
    let created: bool = {
        let mut guard: MutexGuard<'_, Sched> = header.sched.lock();
        let sched: &mut Sched = &mut guard;
        if header.is_ghost() {
            return;
        }
        let cut: bool = chain.contains(header.id);
        match sched.entries.get_mut(&epoch.id) {
            Some(entry) => {
                entry.reflow = true;
                entry.carry = entry.carry.max(stamp);
                entry.writers.union(chain);
                entry.cut |= cut;
                false
            }
            None => {
                let owes: bool = sched.owes(epoch.id);
                epoch.open_token();
                let mut entry: Entry = Entry::new(epoch.clone());
                entry.reflow = true;
                entry.carry = stamp;
                entry.writers = chain.clone();
                entry.cut = cut;
                sched.entries.insert(epoch.id, entry);
                !owes
            }
        }
    };
    if created {
        frame(node, epoch, work);
    }
}

/// A node's own item delivered in a later poll: its children run in the child epoch.
pub(crate) struct OwnItem<'a> {
    pub(crate) node: &'a NodeRef,
    pub(crate) stamp: u64,
}

fn upgrade_sorted(list: Vec<(crate::node::WeakNodeRef, NodeId, u64)>) -> Vec<(NodeRef, u64)> {
    let mut out: Vec<(NodeRef, u64)> = Vec::with_capacity(list.len());
    for (weak, _, stamp) in list {
        if let Some(node) = weak.upgrade() {
            match out.iter_mut().find(|(existing, _): &&mut (NodeRef, u64)| existing.header().id == node.header().id) {
                Some((_, existing)) => *existing = (*existing).max(stamp),
                None => out.push((node, stamp)),
            }
        }
    }
    out.sort_by_key(|(node, _): &(NodeRef, u64)| node.header().id);
    out
}

fn upgrade_nodes(list: Vec<crate::node::WeakNodeRef>) -> Vec<NodeRef> {
    let mut out: Vec<NodeRef> = list.into_iter().filter_map(|weak: crate::node::WeakNodeRef| weak.upgrade()).collect();
    out.sort_by_key(|node: &NodeRef| node.header().id);
    out.dedup_by_key(|node: &mut NodeRef| node.header().id);
    out
}

/// Delivers a batch into `targets`, each an epoch with its base writer set (spec §7.1, §7.4);
/// `writer`'s writes route against the base set plus itself, its creations seed with it removed.
pub(crate) fn deliver_into(
    batch: Batch,
    targets: &[(Arc<Epoch>, WriterSet)],
    writer: Option<NodeId>,
    own: Option<OwnItem<'_>>,
) {
    let Batch { writes, reflows, creations, reruns, refreshes, defers } = batch;
    let writes: Vec<(NodeRef, u64)> = upgrade_sorted(writes);
    let reflows: Vec<(NodeRef, u64)> = upgrade_sorted(reflows);
    let creations: Vec<NodeRef> = upgrade_nodes(creations);
    let reruns: Vec<NodeRef> = upgrade_nodes(reruns);
    let refreshes: Vec<NodeRef> = upgrade_nodes(refreshes);
    let mut work: Work = Work::default();

    // Framing: the whole downstream region opens its entries before anything is released.
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
            frame(node, epoch, &mut work);
        }
        for (node, stamp) in &reflows {
            if node.header().kind == Kind::Derived {
                seed_reflow(node, epoch, &chain, *stamp, &mut work);
            } else {
                frame(node, epoch, &mut work);
            }
        }
        if let Some(item) = &own {
            frame(item.node, epoch, &mut work);
        }
    }

    // Releases.
    for (epoch, base) in targets {
        let chain: WriterSet = base.with(writer);
        for (node, stamp) in &writes {
            release_children(node, epoch, *stamp, &chain, Some(node.header().site), &mut work);
        }
        for (node, stamp) in &reflows {
            if node.header().kind != Kind::Derived {
                release_children(node, epoch, *stamp, &chain, Some(node.header().site), &mut work);
            }
        }
        if let Some(item) = &own {
            release_children(item.node, epoch, item.stamp, base, None, &mut work);
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
        let epochs: Vec<Arc<Epoch>> =
            targets.iter().map(|(epoch, _): &(Arc<Epoch>, WriterSet)| epoch.clone()).collect();
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
    close_all(deliver_outside_deferred(batch, scope));
}

/// Like `deliver_outside`, but hands the epoch's guard token back to the caller.
#[must_use = "the returned tokens must be closed with close_all"]
pub(crate) fn deliver_outside_deferred(batch: Batch, scope: &Arc<Scope>) -> Vec<Arc<Epoch>> {
    if batch.is_empty() {
        return Vec::new();
    }
    let epoch: Arc<Epoch> = Epoch::root(scope);
    scope.set_last(&epoch);
    deliver_into(batch, &[(epoch.clone(), WriterSet::new())], None, None);
    vec![epoch]
}

/// Delivers a run's later batch as one child epoch per epoch it serves; returns them still
/// holding their guard token, which the caller closes with `close_all`.
#[must_use = "the returned tokens must be closed with close_all"]
pub(crate) fn deliver_children(
    node: &NodeRef,
    served: &[(Arc<Epoch>, WriterSet)],
    batch: Batch,
    own_stamp: u64,
) -> Vec<Arc<Epoch>> {
    if batch.is_empty() && own_stamp == 0 {
        return Vec::new();
    }
    let children: Vec<(Arc<Epoch>, WriterSet)> = served
        .iter()
        .map(|(epoch, writers): &(Arc<Epoch>, WriterSet)| (Epoch::child(epoch), writers.clone()))
        .collect();
    let own: Option<OwnItem<'_>> = (own_stamp > 0).then_some(OwnItem { node, stamp: own_stamp });
    deliver_into(batch, &children, Some(node.header().id), own);
    children.into_iter().map(|(epoch, _)| epoch).collect()
}

/// Runs the deferred functions once every one of `epochs` has settled (spec §6.3).
pub(crate) fn attach_defers(epochs: Vec<Arc<Epoch>>, defers: Vec<Deferred>) {
    if epochs.is_empty() {
        for deferred in defers {
            fire_deferred(deferred);
        }
        return;
    }
    let remaining: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(epochs.len()));
    let pending: Arc<Mutex<Option<Vec<Deferred>>>> = Arc::new(Mutex::new(Some(defers)));
    for epoch in epochs {
        let remaining: Arc<AtomicUsize> = remaining.clone();
        let pending: Arc<Mutex<Option<Vec<Deferred>>>> = pending.clone();
        epoch.on_settle(Box::new(move || {
            if remaining.fetch_sub(1, Ordering::SeqCst) == 1 {
                let list: Option<Vec<Deferred>> = pending.lock().take();
                for deferred in list.into_iter().flatten() {
                    fire_deferred(deferred);
                }
            }
        }));
    }
}

/// Starts a deferred function as a lithia task in its scope, with an empty writer set.
pub(crate) fn fire_deferred(deferred: Deferred) {
    let Deferred { f, scope, runtime } = deferred;
    let task_scope: Arc<Scope> = scope.clone();
    let future = async move {
        TaskFuture::new(task_scope, async move { f() }).await;
        scope.dec();
    };
    #[cfg(lithia_loom)]
    {
        let _ = runtime;
        crate::model::spawn(Box::pin(future));
    }
    #[cfg(not(lithia_loom))]
    match runtime {
        Some(handle) => {
            drop(handle.spawn(future));
        }
        None => {
            std::thread::spawn(move || {
                let runtime: tokio::runtime::Runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("lithia: cannot build a runtime for a deferred function");
                runtime.block_on(future);
            });
        }
    }
}

/// Links a new node to its parents and opens entries for every epoch a parent still owes
/// a release in (spec §11.4 step 7). Returns whether every parent had been torn down as a ghost.
pub(crate) fn link(node: &NodeRef, parents: &[NodeRef]) -> bool {
    let header: &Header = node.header();
    let weak: crate::node::WeakNodeRef = Arc::downgrade(node);
    let mut all_closed: bool = true;
    let mut guard: MutexGuard<'_, Sched> = header.sched.lock();
    for parent in parents {
        let parent_header: &Header = parent.header();
        let epochs: Vec<Arc<Epoch>> = {
            let mut parent_guard: MutexGuard<'_, Sched> = parent_header.sched.lock();
            let parent_sched: &mut Sched = &mut parent_guard;
            all_closed &= parent_sched.closed;
            parent_sched.subscribers.push((header.id, weak.clone()));
            let mut list: Vec<Arc<Epoch>> =
                parent_sched.entries.values().map(|entry: &Entry| entry.epoch.clone()).collect();
            if let Some(slot) = &parent_sched.run {
                list.extend(slot.served.iter().filter(|s: &&Served| !s.released).map(|s: &Served| s.epoch.clone()));
            }
            list.extend(
                parent_sched.inherited.iter().filter(|s: &&Served| !s.released).map(|s: &Served| s.epoch.clone()),
            );
            list
        };
        let Some(index) = header.parent_index(parent_header.id) else {
            continue;
        };
        for epoch in epochs {
            let entry: &mut Entry = guard.entries.entry(epoch.id).or_insert_with(|| {
                epoch.open_token();
                Entry::new(epoch.clone())
            });
            entry.waiting.set(index);
        }
    }
    drop(guard);
    settle_wait::bump();
    all_closed
}

/// How a created node stands with its creator when a run of its would start.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// No creator holds it: it was made outside a run, or let go of.
    Free,
    /// Its creator has decided it.
    Kept,
    /// Its creator may still replace or keep it: it waits, registered with the creator.
    Gated,
}

/// How `node` stands with `creator`; a wait is registered when gated. Locks the creator inside the
/// node's lock: creators are older, as parents are in `link`. A release landing afterwards resumes it.
fn standing(node: &NodeRef, creator: Option<&NodeRef>) -> Standing {
    let Some(creator) = creator else {
        return Standing::Free;
    };
    let header: &Header = node.header();
    let mut guard: MutexGuard<'_, Sched> = creator.header().sched.lock();
    if header.held_by.lock().is_none() {
        return Standing::Free;
    }
    if header.is_ghost() || !guard.decides(header) {
        return Standing::Kept;
    }
    if !guard.gated.iter().any(|(id, _): &(NodeId, crate::node::WeakNodeRef)| *id == header.id) {
        guard.gated.push((header.id, Arc::downgrade(node)));
    }
    Standing::Gated
}

/// Starts a queued run: takes over the runnable entries and the handover (spec §7.3 item 1).
/// Returns whether it started, and tokens for the caller to close.
pub(crate) fn start_run(node: &NodeRef, shared: &Arc<RunShared>) -> (bool, Vec<Arc<Epoch>>) {
    let header: &Header = node.header();
    let mut work: Work = Work::default();
    let creator: Option<NodeRef> =
        header.creator.and_then(|_: NodeId| header.held_by.lock().as_ref().and_then(crate::node::WeakNodeRef::upgrade));
    let started: bool = {
        let mut guard: MutexGuard<'_, Sched> = header.sched.lock();
        let sched: &mut Sched = &mut guard;
        sched.queued = false;
        let standing: Standing = if sched.run.is_some() || header.creator.is_none() {
            Standing::Free
        } else {
            standing(node, creator.as_ref())
        };
        let started: bool = if sched.run.is_some() {
            false
        } else if standing == Standing::Gated {
            // Its creator decides it first: it starts again once the creator has moved on.
            false
        } else {
            let mut served: Vec<Served> = std::mem::take(&mut sched.inherited);
            let ready: Vec<EpochId> = sched
                .entries
                .iter()
                .filter(|(_, entry): &(&EpochId, &Entry)| entry.runnable && entry.waiting.is_empty())
                .map(|(id, _): (&EpochId, &Entry)| *id)
                .collect();
            for epoch in ready {
                if let Some(entry) = sched.entries.remove(&epoch) {
                    merge_served(
                        &mut served,
                        Served {
                            epoch: entry.epoch,
                            writers: entry.writers,
                            tokens: 1,
                            released: false,
                            carry: entry.carry,
                        },
                    );
                }
            }
            let killed: bool =
                header.kind == Kind::AsyncSource && header.runs.load(Ordering::SeqCst) != shared.expected();
            let fragile: Option<Vec<Arc<Failure>>> = match header.fragility {
                Some(fragility) if !served.is_empty() && !header.is_ghost() && !killed => {
                    fragility_causes(node, fragility)
                }
                _ => None,
            };
            // Let go of by its creator, with nothing left that can reach it: it is being freed, and
            // only this task keeps it alive for now (spec §7.2, Creator order).
            let orphaned: bool = standing == Standing::Free
                && header.creator.is_some()
                && header.anchors.load(Ordering::SeqCst) == 0
                && header.captured.load(Ordering::SeqCst) == 0
                && sched.subscribers.is_empty();
            if served.is_empty() {
                false
            } else if header.is_ghost() || killed || orphaned {
                close_served(node, served, &mut work);
                false
            } else if let Some(causes) = fragile {
                // Decided before a parent failed: the node fails instead, as `decide` would now.
                fail_by_fragility(header, sched, causes, &mut work);
                close_served(node, served, &mut work);
                false
            } else {
                sched.ran_at = ids::now();
                let id: u64 = if header.kind == Kind::AsyncSource {
                    shared.expected()
                } else {
                    header.runs.fetch_add(1, Ordering::SeqCst) + 1
                };
                shared.set_id(id);
                shared.set_scope(served[0].epoch.scope.clone());
                header.set_bits(phase::PENDING);
                header.clear_bits(phase::FAILED);
                header.failure.store(None);
                sched.failed_by_fragility = false;
                sched.run = Some(RunSlot { id, shared: shared.clone(), served, restart: false });
                sched.release_generation(&mut work.released);
                true
            }
        };
        wake_gated(sched, &mut work);
        started
    };
    drop(creator);
    if started {
        diag::count_run(header);
    }
    (started, drain_deferred(work))
}

/// Releases unreleased records unchanged and closes their tokens.
fn close_served(node: &NodeRef, served: Vec<Served>, work: &mut Work) {
    for record in served {
        if !record.released {
            work.releases.push_back(Release {
                from: node.clone(),
                epoch: record.epoch.clone(),
                carry: record.carry,
                writers: record.writers.clone(),
                origin: None,
            });
        }
        for _ in 0..record.tokens {
            work.closes.push(record.epoch.clone());
        }
    }
}

/// How a run's poll ended.
pub(crate) enum Outcome {
    /// The body returned.
    Ended,
    /// The body yielded and is rescheduled.
    Yielded,
    /// The body awaits something else.
    Suspended,
    /// The body panicked.
    Panicked(String),
}

/// Delivers a poll's batch and, at the first yield or suspension, releases the children.
/// Returns tokens for the caller to close once it holds no strong reference to the node.
#[must_use = "the returned tokens must be closed with close_all"]
pub(crate) fn after_poll(
    node: &NodeRef,
    shared: &Arc<RunShared>,
    batch: Batch,
    outcome: &Outcome,
    own_stamp: u64,
) -> Vec<Arc<Epoch>> {
    let header: &Header = node.header();
    if let Outcome::Panicked(message) = outcome {
        diag::run_panicked(header.site, message);
        header.fail(Failure { message: message.clone(), site: *header.site, causes: Vec::new() });
    }
    let snapshot: Option<(ServedEpochs, bool)> = {
        let guard: MutexGuard<'_, Sched> = header.sched.lock();
        match &guard.run {
            Some(slot) if slot.id == shared.id() => Some((
                slot.served.iter().map(|s: &Served| (s.epoch.clone(), s.writers.clone())).collect(),
                slot.restart || shared.is_cancelled(),
            )),
            _ => None,
        }
    };
    let Some((served, cancelled)) = snapshot else {
        // The run was torn down meanwhile; its writes are applied and still propagate.
        return deliver_outside_deferred(batch, &shared.scope());
    };
    let first: bool = !shared.first_done();
    let mut pending: Vec<Arc<Epoch>> = Vec::new();
    if first {
        shared.set_first_done();
        deliver_into(batch, &served, Some(header.id), None);
    } else {
        let stamp: u64 = if cancelled { 0 } else { own_stamp };
        pending.extend(deliver_children(node, &served, batch, stamp));
    }
    let closes_tokens: bool = !matches!(outcome, Outcome::Yielded);
    if cancelled {
        if !matches!(outcome, Outcome::Yielded | Outcome::Suspended) {
            pending.extend(finalize(node, shared, End::Finished));
        }
        return pending;
    }
    let mut work: Work = Work::default();
    {
        let mut guard: MutexGuard<'_, Sched> = header.sched.lock();
        let sched: &mut Sched = &mut guard;
        let Sched { run, entries, .. } = sched;
        if let Some(slot) = run.as_mut().filter(|slot: &&mut RunSlot| slot.id == shared.id()) {
            for record in slot.served.iter_mut() {
                if first && !record.released {
                    record.released = true;
                    record.carry = record.carry.max(own_stamp);
                    match entries.get_mut(&record.epoch.id) {
                        Some(entry) => {
                            entry.carry = entry.carry.max(record.carry);
                            entry.writers.union(&record.writers);
                        }
                        None => work.releases.push_back(Release {
                            from: node.clone(),
                            epoch: record.epoch.clone(),
                            carry: record.carry,
                            writers: record.writers.clone(),
                            origin: None,
                        }),
                    }
                }
                if closes_tokens {
                    for _ in 0..record.tokens {
                        work.closes.push(record.epoch.clone());
                    }
                    record.tokens = 0;
                }
            }
        }
    }
    pending.extend(drain_deferred(work));
    if matches!(outcome, Outcome::Ended | Outcome::Panicked(_)) {
        pending.extend(finalize(node, shared, End::Finished));
    }
    pending
}

/// Why a run stopped.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum End {
    /// The body returned or panicked, or the run was aborted at a poll boundary.
    Finished,
    /// The node's runtime shut down.
    Shutdown,
}

/// Ends a run: hands its epochs to the next run if superseded, otherwise closes them.
/// Returns tokens for the caller to close once it holds no strong reference to the node.
#[must_use = "the returned tokens must be closed with close_all"]
pub(crate) fn finalize(node: &NodeRef, shared: &Arc<RunShared>, end: End) -> Vec<Arc<Epoch>> {
    let header: &Header = node.header();
    let mut work: Work = Work::default();
    {
        let mut guard: MutexGuard<'_, Sched> = header.sched.lock();
        let sched: &mut Sched = &mut guard;
        let slot: RunSlot = match sched.run.take() {
            Some(slot) if slot.id == shared.id() => slot,
            other => {
                sched.run = other;
                return Vec::new();
            }
        };
        let restart: bool = slot.restart && end == End::Finished && !header.is_ghost();
        if restart || shared.is_cancelled() {
            sched.release_generation(&mut work.released);
        }
        if !restart {
            // The run ended without a successor: keyed nodes it did not reclaim are let go.
            sched.holdover.drain_into(&mut work.released);
        }
        if !work.released.is_empty() {
            sched.releasing = true;
        }
        if restart {
            for record in slot.served {
                merge_served(&mut sched.inherited, record);
            }
            if !sched.queued {
                sched.queued = true;
                work.spawns.push(node.clone());
            }
        } else {
            let Sched { entries, .. } = &mut *sched;
            for record in slot.served {
                if !record.released {
                    match entries.get_mut(&record.epoch.id) {
                        Some(entry) => {
                            entry.carry = entry.carry.max(record.carry);
                            entry.writers.union(&record.writers);
                        }
                        None => work.releases.push_back(Release {
                            from: node.clone(),
                            epoch: record.epoch.clone(),
                            carry: record.carry,
                            writers: record.writers.clone(),
                            origin: None,
                        }),
                    }
                }
                for _ in 0..record.tokens {
                    work.closes.push(record.epoch.clone());
                }
            }
            header.clear_bits(phase::PENDING);
            let follow_up: bool =
                sched.entries.values().any(|entry: &Entry| entry.runnable && entry.waiting.is_empty());
            if end == End::Finished && follow_up && !sched.queued && !header.is_ghost() {
                sched.queued = true;
                work.spawns.push(node.clone());
            }
        }
        wake_gated(sched, &mut work);
        header.under_way.store(true, Ordering::SeqCst);
    }
    let releasing: bool = !work.released.is_empty();
    let closes: Vec<Arc<Epoch>> = drain_deferred(work);
    if releasing {
        header.sched.lock().releasing = false;
        crate::settle_wait::bump();
    }
    closes
}

/// Gives up on a node whose run task was dropped because its runtime shut down (spec §11.3).
#[must_use = "the returned tokens must be closed with close_all"]
pub(crate) fn abandon(node: &NodeRef, shared: &Arc<RunShared>) -> Vec<Arc<Epoch>> {
    let mut pending: Vec<Arc<Epoch>> = finalize(node, shared, End::Shutdown);
    let header: &Header = node.header();
    let mut work: Work = Work::default();
    {
        let mut guard: MutexGuard<'_, Sched> = header.sched.lock();
        let sched: &mut Sched = &mut guard;
        if sched.run.is_some() {
            return pending;
        }
        sched.queued = false;
        let ready: Vec<EpochId> = sched
            .entries
            .iter()
            .filter(|(_, entry): &(&EpochId, &Entry)| entry.runnable)
            .map(|(id, _): (&EpochId, &Entry)| *id)
            .collect();
        for epoch in ready {
            settle(node, sched, epoch, &mut work);
        }
        let inherited: Vec<Served> = std::mem::take(&mut sched.inherited);
        close_served(node, inherited, &mut work);
    }
    if !header.is_ghost() {
        header.fail(Failure { message: "runtime shut down".to_string(), site: *header.site, causes: Vec::new() });
    }
    pending.extend(drain_deferred(work));
    pending
}

/// Stops the async block or generator feeding a source node after a write (spec §7.3 item 5).
pub(crate) fn kill_run(node: &NodeRef) {
    let shared: Option<Arc<RunShared>> = {
        let guard: MutexGuard<'_, Sched> = node.header().sched.lock();
        guard.run.as_ref().map(|slot: &RunSlot| slot.shared.clone())
    };
    if let Some(shared) = shared {
        shared.cancel();
        diag::Diag::Superseded { site: node.header().site, run: shared.id() }.emit();
        shared.wake();
    }
}

/// Settles every entry and run of a node that can no longer run; used by disposal.
pub(crate) fn retire(node: &NodeRef) {
    let header: &Header = node.header();
    let mut work: Work = Work::default();
    let shared: Option<Arc<RunShared>> = {
        let mut guard: MutexGuard<'_, Sched> = header.sched.lock();
        let sched: &mut Sched = &mut guard;
        let epochs: Vec<EpochId> = sched.entries.keys().copied().collect();
        for epoch in epochs {
            settle(node, sched, epoch, &mut work);
        }
        let inherited: Vec<Served> = std::mem::take(&mut sched.inherited);
        close_served(node, inherited, &mut work);
        sched.queued = false;
        wake_gated(sched, &mut work);
        match sched.run.take() {
            Some(slot) => {
                let shared: Arc<RunShared> = slot.shared.clone();
                close_served(node, slot.served, &mut work);
                Some(shared)
            }
            None => None,
        }
    };
    if let Some(shared) = shared {
        shared.cancel();
        shared.wake();
    }
    drain(work);
}
