// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! A run's life: start, each poll's delivery and release, supersession, the end, and retirement (spec §7.3, §8).

use super::*;

/// Starts a queued run: takes over the runnable entries and the handover (spec §7.3 item 1).
/// Returns whether it started, and tokens for the caller to close.
pub(crate) fn start_run(node: &NodeRef, shared: &Arc<RunControl>) -> (bool, Tokens) {
    let header: &Header = node.header();
    let mut work: Work = Work::default();
    let creator: Option<NodeRef> = header.creator.and_then(|_: NodeId| {
        header.lifetime.held_by.lock().as_ref().and_then(crate::graph::node::WeakNodeRef::upgrade)
    });
    // Epochs a cut rerun read its writes from, dropped once the lock is released.
    let mut suppress_from: Epochs = Vec::new();
    let mut too_fast: Option<u64> = None;
    let started: bool = {
        let mut guard: SchedGuard<'_> = header.lock_sched();
        let sched: &mut Sched = &mut guard;
        sched.queued = false;
        let standing: Standing = if sched.run.is_some() || header.creator.is_none() {
            Standing::Free
        } else {
            standing(node, creator.as_ref())
        };
        let started: bool = if sched.run.is_some() || whole_epochs_wait(header, sched) {
            // A `batches = WHOLE` node framed again meanwhile starts once that epoch is ready too.
            false
        } else if standing == Standing::Gated {
            // Its creator decides it first: it starts again once the creator has moved on.
            false
        } else {
            let protected: bool = !sched.handover.is_empty();
            let mut served: ServedList = std::mem::take(&mut sched.handover);
            sched.entries.take_where(
                |entry: &Entry| entry.is_ready(),
                |entry: Entry| {
                    let record: Served = Served {
                        epoch: entry.epoch,
                        writers: entry.writers,
                        tokens: 1,
                        released: false,
                        carry: entry.carry,
                    };
                    merge_served(&mut served, record);
                },
            );
            let killed: bool =
                header.kind == Kind::AsyncSource && header.runs.load(Ordering::SeqCst) != shared.expected();
            let failing: Option<Causes> = if !served.is_empty() && !header.is_ghost() && !killed {
                parent_failures(node, header.shape.resilience)
            } else {
                None
            };
            // Let go of by its creator, with nothing left that can reach it: it is being freed, and
            // only this task keeps it alive for now (spec §7.2, Creator order).
            let orphaned: bool = standing == Standing::Free
                && header.creator.is_some()
                && header.lifetime.anchors.load(Ordering::SeqCst) == 0
                && header.lifetime.captured.load(Ordering::SeqCst) == 0
                && sched.subscribers.is_empty();
            if served.is_empty() {
                false
            } else if header.is_ghost() || killed || orphaned {
                close_served(node, sched, served, &mut work);
                false
            } else if let Some(causes) = failing {
                // Decided before a parent failed: the node fails instead, as `decide` would now.
                fail_by_parents(header, sched, causes, &mut work);
                close_served(node, sched, served, &mut work);
                false
            } else {
                sched.ran_at = ids::now();
                sched.whole_deferred = 0;
                let id: RunId = if header.kind == Kind::AsyncSource {
                    shared.expected()
                } else {
                    header.runs.advance(Ordering::SeqCst)
                };
                shared.set_id(id);
                shared.set_scope(served[0].epoch.scope.clone());
                if !sched.suppress_in.is_empty() {
                    // What the lineage wrote there by now: a run cut by a write from its own task
                    // may record its earlier writes only after the cut (spec §7.4, Case AA).
                    suppress_from = std::mem::take(&mut sched.suppress_in);
                    let mut suppress: Vec<NodeId> = Vec::new();
                    for epoch in &suppress_from {
                        for written in epoch.written_by(header.writer) {
                            if !suppress.contains(&written) {
                                suppress.push(written);
                            }
                        }
                    }
                    shared.set_suppress(suppress);
                }
                header.begin_run();
                too_fast = sched.rate.count();
                sched.failed_by_parents = false;
                sched.run = Some(RunSlot { id, shared: shared.clone(), served, restart: false, protected });
                sched.take_generation(&mut work.released);
                true
            }
        };
        wake_gated(sched, &mut work);
        started
    };
    drop(creator);
    drop(suppress_from);
    if let Some(observed) = too_fast {
        diag::rerun_too_fast(header.site, observed);
    }
    (started, drain_deferred(work))
}

/// Releases unreleased records unchanged and closes their tokens; `sched` is the node's, locked.
pub(super) fn close_served(node: &NodeRef, sched: &mut Sched, served: ServedList, work: &mut Work) {
    let mut released: usize = 0;
    for record in served {
        if !record.released {
            released += 1;
            work.release(Release::unaddressed(node, &record));
        }
        work.closes.add(record.epoch.clone(), record.tokens);
    }
    address_releases(sched, work, released);
}

/// Gives the last `count` releases in `work`, gathered under the lock of `sched`'s node, that
/// node's live subscribers: the children those obligations counted (spec §11.4, step 4).
pub(super) fn address_releases(sched: &mut Sched, work: &mut Work, count: usize) {
    if count == 0 {
        return;
    }
    let mut children: Children = sched.children();
    let first: usize = work.releases.len() - count;
    for (at, release) in work.releases[first..].iter_mut().enumerate() {
        release.children = if at + 1 == count { std::mem::take(&mut children) } else { children.clone() };
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
pub(crate) fn after_poll(
    node: &NodeRef,
    shared: &Arc<RunControl>,
    batch: Batch,
    outcome: &Outcome,
    own_stamp: Stamp,
) -> Tokens {
    let header: &Header = node.header();
    debug_assert!(
        shared.id() <= header.runs.load(Ordering::Relaxed),
        "lithia: a run's id is ahead of its node's count"
    );
    if let Outcome::Panicked(message) = outcome {
        diag::run_panicked(header.site, message);
        // Only the current run fails the node, as only it publishes: writes and run starts are held off.
        let _write: MutexGuard<'_, ()> = header.write.lock();
        let guard: SchedGuard<'_> = header.lock_sched();
        let current: bool = !shared.is_cancelled()
            && !header.is_ghost()
            && header.runs.load(Ordering::SeqCst) == shared.id()
            && guard.run.as_ref().is_some_and(|slot: &RunSlot| slot.id == shared.id());
        if current {
            header.fail(Failure::new(message.clone(), *header.site));
        }
    }
    let first: bool = !shared.first_done();
    // The served epochs are copied only when there is something to deliver into them.
    let delivers: bool = !batch.is_empty() || (!first && own_stamp.is_some());
    let snapshot: Option<(ServedEpochs, ServedEpochs, bool)> = {
        let guard: SchedGuard<'_> = header.lock_sched();
        match &guard.run {
            Some(slot) if slot.id == shared.id() => Some((
                if delivers {
                    slot.served.iter().map(|s: &Served| (s.epoch.clone(), s.writers.clone())).collect()
                } else {
                    Vec::new()
                },
                // A first item's epochs an earlier run released already; usually none.
                if first && own_stamp.is_some() {
                    slot.served
                        .iter()
                        .filter(|s: &&Served| s.released)
                        .map(|s: &Served| (s.epoch.clone(), s.writers.clone()))
                        .collect()
                } else {
                    Vec::new()
                },
                slot.restart || shared.is_cancelled(),
            )),
            _ => None,
        }
    };
    let Some((served, released, cancelled)) = snapshot else {
        // The run was torn down meanwhile; its writes are applied and still propagate.
        return deliver_outside_deferred(batch, &shared.scope());
    };
    let mut pending: Tokens = Tokens::new();
    if first {
        shared.set_first_done();
        deliver_into(batch, &served, Some(header.writer), None);
        if !released.is_empty() {
            // A published item reaches the children of epochs released already as a later item does.
            pending.append(deliver_children(node, &released, Batch::default(), own_stamp));
        }
    } else {
        // A published item is delivered even if the run was superseded since it published.
        pending.append(deliver_children(node, &served, batch, own_stamp));
    }
    if matches!(outcome, Outcome::Yielded) && !cancelled {
        protect_ends(node, shared);
    }
    let closes_tokens: bool = !matches!(outcome, Outcome::Yielded);
    if cancelled {
        if first && own_stamp.is_some() {
            carry_published(node, shared, own_stamp);
        }
        if !matches!(outcome, Outcome::Yielded | Outcome::Suspended) {
            pending.append(finalize(node, shared, End::Finished));
        }
        return pending;
    }
    let mut work: Work = Work::default();
    {
        let mut guard: SchedGuard<'_> = header.lock_sched();
        let sched: &mut Sched = &mut guard;
        let mut released: usize = 0;
        let Sched { run, entries, .. } = &mut *sched;
        if let Some(slot) = run.as_mut().filter(|slot: &&mut RunSlot| slot.id == shared.id()) {
            for record in slot.served.iter_mut() {
                if first && !record.released {
                    record.released = true;
                    record.carry = record.carry.max(own_stamp);
                    released += usize::from(release_or_fold(entries, node, record, &mut work));
                }
                if closes_tokens {
                    work.closes.add(record.epoch.clone(), record.tokens);
                    record.tokens = 0;
                }
            }
        }
        address_releases(sched, &mut work, released);
    }
    pending.append(drain_deferred(work));
    if matches!(outcome, Outcome::Ended | Outcome::Panicked(_)) {
        pending.append(finalize(node, shared, End::Finished));
    }
    pending
}

/// A superseded run published its first item: the run that takes its epochs over releases them
/// with the item's stamp, so the children see the change even if the new run's item is equal.
pub(super) fn carry_published(node: &NodeRef, shared: &Arc<RunControl>, stamp: Stamp) {
    let mut guard: SchedGuard<'_> = node.header().lock_sched();
    if let Some(slot) = guard.run.as_mut().filter(|slot: &&mut RunSlot| slot.id == shared.id()) {
        for record in slot.served.iter_mut().filter(|record: &&mut Served| !record.released) {
            record.carry = record.carry.max(stamp);
        }
    }
}

/// A run that protected another produced its first item: input that waited for it supersedes it now.
pub(super) fn protect_ends(node: &NodeRef, shared: &Arc<RunControl>) {
    let header: &Header = node.header();
    let mut work: Work = Work::default();
    {
        let mut guard: SchedGuard<'_> = header.lock_sched();
        let sched: &mut Sched = &mut guard;
        let ran_at: Stamp = sched.ran_at;
        // As in `decide`: an entry runnable only for a reflow waits for the run to end.
        let waiting: bool = sched.entries.values().any(|entry: &Entry| entry.is_ready() && !entry.reflow_only(ran_at));
        if let Some(slot) = sched.run.as_mut().filter(|slot: &&mut RunSlot| slot.id == shared.id() && slot.protected) {
            slot.protected = false;
            // The run itself is polling, so it needs no wake.
            if waiting && !slot.restart {
                slot.supersede(header.site, &mut work);
            }
        }
    }
    drain(work);
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
pub(crate) fn finalize(node: &NodeRef, shared: &Arc<RunControl>, end: End) -> Tokens {
    let header: &Header = node.header();
    let mut work: Work = Work::default();
    {
        let mut guard: SchedGuard<'_> = header.lock_sched();
        let sched: &mut Sched = &mut guard;
        let slot: RunSlot = match sched.run.take() {
            Some(slot) if slot.id == shared.id() => slot,
            other => {
                sched.run = other;
                return Tokens::new();
            }
        };
        let restart: bool = slot.restart && end == End::Finished && !header.is_ghost();
        if restart || shared.is_cancelled() {
            sched.take_generation(&mut work.released);
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
                merge_served(&mut sched.handover, record);
            }
            if !sched.queued {
                sched.queued = true;
                work.spawns.push(node.clone());
            }
        } else {
            let mut released: usize = 0;
            for record in slot.served {
                if !record.released {
                    released += usize::from(release_or_fold(&mut sched.entries, node, &record, &mut work));
                }
                work.closes.add(record.epoch.clone(), record.tokens);
            }
            address_releases(sched, &mut work, released);
            header.clear_bits(phase::PENDING);
            let follow_up: bool =
                !whole_epochs_wait(header, sched) && sched.entries.values().any(|entry: &Entry| entry.is_ready());
            if end == End::Finished && follow_up && !sched.queued && !header.is_ghost() {
                sched.queued = true;
                work.spawns.push(node.clone());
            }
        }
        wake_gated(sched, &mut work);
        header.diag.under_way.store(true, Ordering::Release);
    }
    let releasing: bool = !work.released.is_empty();
    let closes: Tokens = drain_deferred(work);
    if releasing {
        header.lock_sched().releasing = false;
        crate::testing::progress::bump();
    }
    closes
}

/// Gives up on a node whose run task was dropped because its runtime shut down (spec §11.3).
pub(crate) fn abandon(node: &NodeRef, shared: &Arc<RunControl>) -> Tokens {
    let mut pending: Tokens = finalize(node, shared, End::Shutdown);
    let header: &Header = node.header();
    let mut work: Work = Work::default();
    {
        let mut guard: SchedGuard<'_> = header.lock_sched();
        let sched: &mut Sched = &mut guard;
        if sched.run.is_some() {
            return pending;
        }
        sched.queued = false;
        let ready: Vec<EpochId> = sched
            .entries
            .iter()
            .filter(|(_, entry): &(&EpochId, &Entry)| entry.due.contains(Due::RUNNABLE))
            .map(|(id, _): (&EpochId, &Entry)| *id)
            .collect();
        for epoch in ready {
            settle(node, sched, epoch, &mut work);
        }
        let handover: ServedList = std::mem::take(&mut sched.handover);
        close_served(node, sched, handover, &mut work);
    }
    if !header.is_ghost() {
        header.fail(Failure::new("runtime shut down", *header.site));
    }
    pending.append(drain_deferred(work));
    pending
}

/// Stops the async block or generator feeding a source node after a write (spec §7.3 item 5).
pub(crate) fn kill_run(node: &NodeRef) {
    let shared: Option<Arc<RunControl>> = {
        let guard: SchedGuard<'_> = node.header().lock_sched();
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
    let shared: Option<Arc<RunControl>> = {
        let mut guard: SchedGuard<'_> = header.lock_sched();
        let sched: &mut Sched = &mut guard;
        let epochs: Vec<EpochId> = sched.entries.keys().copied().collect();
        for epoch in epochs {
            settle(node, sched, epoch, &mut work);
        }
        let handover: ServedList = std::mem::take(&mut sched.handover);
        close_served(node, sched, handover, &mut work);
        sched.queued = false;
        wake_gated(sched, &mut work);
        match sched.run.take() {
            Some(slot) => {
                let shared: Arc<RunControl> = slot.shared.clone();
                close_served(node, sched, slot.served, &mut work);
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
