// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! A step's gathered work, carried out once its locks are released (spec §11.4, step 1).

use super::*;

/// Runs to wake once a step's locks are released.
pub(super) type Wakes = Vec<Arc<RunControl>>;

/// A parent telling its waiting children that it settled an epoch.
pub(crate) struct Release {
    pub(super) from: NodeRef,
    /// The parent's subscribers when the release was gathered: those its obligation counted.
    pub(super) children: Children,
    pub(super) epoch: Arc<Epoch>,
    /// Change stamp to compare against each child's last run, or 0.
    pub(super) carry: Stamp,
    pub(super) writers: WriterSet,
    pub(super) origin: Option<&'static Site>,
}

impl Release {
    /// A run's record released as it stands; `address_releases` fills in the children.
    pub(super) fn unaddressed(from: &NodeRef, record: &Served) -> Release {
        Release {
            from: from.clone(),
            children: Vec::new(),
            epoch: record.epoch.clone(),
            carry: record.carry,
            writers: record.writers.clone(),
            origin: None,
        }
    }
}

/// Folds an unreleased record into its epoch's open entry, whose close releases it, or else releases
/// it now; true if released, for `address_releases` to count.
pub(super) fn release_or_fold(entries: &mut Entries, from: &NodeRef, record: &Served, work: &mut Work) -> bool {
    match entries.get_mut(&record.epoch.id) {
        Some(entry) => {
            entry.carry = entry.carry.max(record.carry);
            entry.writers.union(&record.writers);
            false
        }
        None => {
            work.releases.push_back(Release::unaddressed(from, record));
            true
        }
    }
}

impl RunSlot {
    /// Marks the run in flight to restart and cancels it; the caller wakes it unless it is polling.
    pub(super) fn supersede(&mut self, site: &'static Site, work: &mut Work) {
        self.restart = true;
        self.shared.cancel();
        work.diags.push(Diag::Superseded { site, run: self.id });
    }
}

/// Actions gathered under locks and carried out after unlocking.
#[derive(Default)]
pub(crate) struct Work {
    pub(super) releases: VecDeque<Release>,
    pub(super) spawns: Vec<NodeRef>,
    pub(super) wakes: Wakes,
    pub(super) closes: Tokens,
    pub(super) diags: Vec<Diag>,
    /// Strong references taken from weak lists, dropped only after every lock is released.
    pub(super) keep: Vec<NodeRef>,
    /// Generations released by this step, let go of before its spawns.
    pub(super) released: Vec<NodeRef>,
    /// Created nodes their creator gated, to start again once it has moved on.
    pub(super) resumes: Vec<crate::graph::node::WeakNodeRef>,
    /// Writers cut in an epoch, to rerun once it has settled.
    pub(super) cut_reruns: Vec<CutRerun>,
}

impl Work {
    fn is_drained(&self) -> bool {
        self.releases.is_empty()
            && self.spawns.is_empty()
            && self.wakes.is_empty()
            && self.closes.is_empty()
            && self.diags.is_empty()
            && self.keep.is_empty()
            && self.released.is_empty()
            && self.resumes.is_empty()
            && self.cut_reruns.is_empty()
    }
}

impl Drop for Work {
    /// Carries out what it still holds, so a panic through a step cannot leave a scope unsettled.
    fn drop(&mut self) {
        if self.is_drained() {
            return;
        }
        debug_assert!(std::thread::panicking(), "lithia: protocol work dropped undrained");
        drain(std::mem::take(self));
    }
}

/// Carries out gathered work: releases, spawns and wakes, then token closes.
pub(crate) fn drain(work: Work) {
    drain_deferred(work).close();
}

/// Like `drain`, but hands the token closes back to the caller.
pub(crate) fn drain_deferred(mut work: Work) -> Tokens {
    while let Some(release) = work.releases.pop_front() {
        let Release { from, children, epoch, carry, writers, origin } = release;
        release_children(&from, children, &epoch, carry, &writers, origin, &mut work);
        work.keep.push(from);
    }
    for diag in std::mem::take(&mut work.diags) {
        diag.emit();
    }
    for rerun in std::mem::take(&mut work.cut_reruns) {
        if let Some(epoch) = rerun.epoch.upgrade() {
            epoch.on_settle(Box::new(move || rerun_cut(rerun)));
        }
    }
    {
        // The step's other references and its released generations go before its spawns,
        // so an idle old child released here is freed before its task exists.
        let _op: crate::lifetime::op::Operation = crate::lifetime::op::enter();
        drop(std::mem::take(&mut work.keep));
        crate::lifetime::support::release_generation(std::mem::take(&mut work.released));
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
    crate::exec::fast_path::dispatch(spawns);
    for run in std::mem::take(&mut work.wakes) {
        run.wake();
    }
    let closes: Tokens = std::mem::take(&mut work.closes);
    drop(work);
    progress::bump();
    closes
}

/// Queues a run of an idle node that has work left, as one its creator gated; true if it did.
pub(super) fn queue_if_runnable(node: &NodeRef) -> bool {
    let mut guard: SchedGuard<'_> = node.header().lock_sched();
    let ready: bool = !guard.handover.is_empty() || guard.entries.values().any(|entry: &Entry| entry.is_ready());
    let queue: bool = ready && guard.run.is_none() && !guard.queued && !node.header().is_ghost();
    if queue {
        guard.queued = true;
    }
    queue
}

/// Starts a gated node again, if it has work left (spec §7.2, Creator order).
pub(crate) fn resume(node: &NodeRef) {
    if queue_if_runnable(node) {
        let mut work: Work = Work::default();
        work.spawns.push(node.clone());
        drain(work);
    }
}

/// Hands the nodes this creator gated to `work`, to start again; they gate again if it still decides them.
pub(super) fn wake_gated(sched: &mut Sched, work: &mut Work) {
    work.resumes.extend(sched.gated.drain(..).map(|(_, weak): (NodeId, crate::graph::node::WeakNodeRef)| weak));
}
