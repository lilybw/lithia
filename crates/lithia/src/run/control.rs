// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! A run's shared state: its id, cancellation, waker, stamp and creation places (spec §7.3, §8).

use super::*;

/// Creations so far at each site in one run: an unkeyed node's place in its lineage.
pub(super) type SiteCounts = Vec<(SiteKey, u64)>;

/// What a run's creations share: their places so far, and the lineage they descend from.
#[derive(Default)]
pub(super) struct Creations {
    counts: SiteCounts,
    /// The creator's lineage, fetched at the run's first creation.
    lineage: Option<Arc<Lineage>>,
}

/// State of one run shared between its task, its yielder and the scheduler.
pub(crate) struct RunControl {
    pub(crate) node: WeakNodeRef,
    /// The node's id; none once a run is made for a node already freed.
    pub(crate) node_id: Option<NodeId>,
    /// The scope of the epochs the run serves, set once as it starts; until then, the node's.
    #[allow(clippy::disallowed_types)] // set under the scheduling lock before the body first polls
    pub(super) scope: std::sync::OnceLock<Arc<Scope>>,
    pub(super) home: Arc<Scope>,
    pub(super) id: Atomic<RunId>,
    /// Run id an async source block was registered under at creation, or none.
    pub(super) expected: RunId,
    pub(super) cancelled: AtomicBool,
    pub(super) waker: Mutex<Option<Waker>>,
    pub(super) first_done: AtomicBool,
    pub(super) yielded: AtomicBool,
    /// Newest change stamp of the node's own items during the current poll.
    pub(super) stamp: Atomic<Stamp>,
    /// Creations so far at each site, by site address: an unkeyed node's place in its lineage.
    pub(super) creations: Mutex<Creations>,
    /// Nodes this run's writes and reflows leave alone: a cut writer's rerun (spec §7.4). Set under
    /// the node's scheduling lock before the body first polls, so loom need not see it.
    #[allow(clippy::disallowed_types)]
    pub(super) suppress: std::sync::OnceLock<Vec<NodeId>>,
}

impl RunControl {
    #[allow(clippy::disallowed_types)]
    pub(crate) fn new(node: WeakNodeRef, node_id: Option<NodeId>, scope: Arc<Scope>, expected: RunId) -> RunControl {
        RunControl {
            node,
            node_id,
            scope: std::sync::OnceLock::new(),
            home: scope,
            id: Atomic::new(RunId::NONE),
            expected,
            cancelled: AtomicBool::new(false),
            waker: Mutex::new(None),
            first_done: AtomicBool::new(false),
            yielded: AtomicBool::new(false),
            stamp: Atomic::new(Stamp::NONE),
            creations: Mutex::new(Creations::default()),
            suppress: std::sync::OnceLock::new(),
        }
    }

    /// Sets, once, the nodes this run leaves alone.
    pub(crate) fn set_suppress(&self, nodes: Vec<NodeId>) {
        let _ = self.suppress.set(nodes);
    }

    /// Whether this run leaves writes to `node` alone.
    pub(crate) fn suppresses(&self, node: NodeId) -> bool {
        self.suppress.get().is_some_and(|nodes: &Vec<NodeId>| nodes.contains(&node))
    }

    /// The lineage this run's creations descend from, and for an unkeyed one its place among the
    /// run's creations at `site`; `root` gives the lineage at the run's first creation.
    pub(crate) fn creation(
        &self,
        site: SiteKey,
        keyed: bool,
        root: impl FnOnce() -> Arc<Lineage>,
    ) -> (Arc<Lineage>, u64) {
        let mut guard: MutexGuard<'_, Creations> = self.creations.lock();
        let lineage: Arc<Lineage> = match &guard.lineage {
            Some(lineage) => lineage.clone(),
            None => {
                // Fetched without this lock held, as it takes the creator's scheduling lock.
                drop(guard);
                let lineage: Arc<Lineage> = root();
                guard = self.creations.lock();
                guard.lineage.get_or_insert(lineage).clone()
            }
        };
        if keyed {
            return (lineage, 0);
        }
        let place: u64 = match guard.counts.iter_mut().find(|(known, _): &&mut (SiteKey, u64)| *known == site) {
            Some((_, count)) => {
                *count += 1;
                *count - 1
            }
            None => {
                guard.counts.push((site, 1));
                0
            }
        };
        (lineage, place)
    }

    pub(crate) fn id(&self) -> RunId {
        self.id.load(Ordering::Acquire)
    }

    /// Set under the node's scheduling lock before the run's first poll; a release store suffices.
    pub(crate) fn set_id(&self, id: RunId) {
        self.id.store(id, Ordering::Release);
    }

    pub(crate) fn expected(&self) -> RunId {
        self.expected
    }

    pub(crate) fn scope(&self) -> Arc<Scope> {
        self.scope.get().unwrap_or(&self.home).clone()
    }

    /// Sets the scope as the run starts; a run starts once.
    pub(crate) fn set_scope(&self, scope: Arc<Scope>) {
        let set: Result<(), Arc<Scope>> = self.scope.set(scope);
        debug_assert!(set.is_ok(), "lithia: a run's scope was set twice");
    }

    /// Marks the run superseded or killed; its items are discarded from now on.
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Wakes the run's task so it stops at its next poll boundary.
    pub(crate) fn wake(&self) {
        let waker: Option<Waker> = self.waker.lock().clone();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    pub(super) fn set_waker(&self, waker: &Waker) {
        let mut slot: MutexGuard<'_, Option<Waker>> = self.waker.lock();
        match slot.as_ref() {
            Some(existing) if existing.will_wake(waker) => {}
            _ => *slot = Some(waker.clone()),
        }
    }

    pub(crate) fn first_done(&self) -> bool {
        self.first_done.load(Ordering::Acquire)
    }

    pub(crate) fn set_first_done(&self) {
        self.first_done.store(true, Ordering::Release);
    }

    /// Records a change stamp produced by the node's own item.
    pub(crate) fn note_stamp(&self, stamp: Stamp) {
        self.stamp.fetch_max(stamp, Ordering::SeqCst);
    }

    /// Only the run's own task touches these, during its poll, so the stores need no ordering.
    pub(super) fn begin_poll(&self) {
        self.yielded.store(false, Ordering::Relaxed);
        self.stamp.store(Stamp::NONE, Ordering::Relaxed);
    }
}
