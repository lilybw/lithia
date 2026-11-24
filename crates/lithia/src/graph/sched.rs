// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! A node's scheduling state and its lock: entries, the run slot, the epochs it owes (spec §7, §11.4).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::sync::MutexGuard;

#[allow(unused_imports)]
use super::node::*;
use crate::Site;
use crate::expand::keyed::KeyedSet;
use crate::graph::epoch::{Epoch, Epochs};
use crate::graph::ids::{EpochId, NodeId, RunId, Stamp};
use crate::graph::sets::{ParentMask, WriterSet};
use crate::run::RunControl;

/// Scheduler state of one node, guarded by its scheduling lock.
pub(crate) struct Sched {
    /// Open entries not taken by a run, one per epoch the node is framed in.
    pub(crate) entries: Entries,
    /// The run in flight, if any.
    pub(crate) run: Option<RunSlot>,
    /// Epochs a superseded run served, handed to the next run.
    pub(crate) handover: ServedList,
    /// A run task was spawned and has not started yet.
    pub(crate) queued: bool,
    /// Created, but its creation has not been delivered yet.
    pub(crate) unseeded: bool,
    /// Clock stamp when the last run started.
    pub(crate) ran_at: Stamp,
    /// Children by id, held weakly; a freed child removes itself, without a scan of the others.
    pub(crate) subscribers: Subscribers,
    /// What `lithia::test::settled` follows from this node; only `testing::trace` touches it.
    pub(crate) trace: crate::testing::trace::Trace,
    /// The generation: nodes the runs created without a key, held until the next run is decided.
    pub(crate) generation: Vec<NodeRef>,
    /// The generation's keyed nodes; a release hands them to the holdover.
    pub(crate) keyed: KeyedSet,
    /// Keyed nodes of released generations, held until a run ends without being superseded.
    pub(crate) holdover: KeyedSet,
    /// Disposed: creations under this node get no creator.
    pub(crate) closed: bool,
    /// A stopped run's generation is being released; the node is not quiet until it is.
    pub(crate) releasing: bool,
    /// Failed through fragility rather than its own panic.
    pub(crate) failed_by_fragility: bool,
    /// Nodes this node created that wait for it to decide them before they run.
    pub(crate) gated: Vec<(NodeId, WeakNodeRef)>,
    /// Epochs whose writes by this node's lineage its next run leaves alone, as a cut writer's
    /// rerun does (spec §7.4); read when that run starts, so writes recorded after the cut count.
    pub(crate) suppress_in: Epochs,
    /// Epochs that became ready while a `WHOLE_EPOCHS` node waited, since its last run started.
    pub(crate) whole_deferred: u32,
    /// Run starts in the current window, for the rerun-rate warning (spec §12).
    pub(crate) rate: crate::diag::RateWindow,
}

/// A node's scheduling lock, held; debug builds check the lock's invariants (spec §11.4) as it
/// is released. `let guard: SchedGuard<'_> = header.lock_sched();`
pub(crate) struct SchedGuard<'a>(MutexGuard<'a, Sched>);

impl<'a> SchedGuard<'a> {
    pub(crate) fn new(guard: MutexGuard<'a, Sched>) -> SchedGuard<'a> {
        SchedGuard(guard)
    }
}

impl std::ops::Deref for SchedGuard<'_> {
    type Target = Sched;

    fn deref(&self) -> &Sched {
        &self.0
    }
}

impl std::ops::DerefMut for SchedGuard<'_> {
    fn deref_mut(&mut self) -> &mut Sched {
        &mut self.0
    }
}

impl Drop for SchedGuard<'_> {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        if !std::thread::panicking() {
            self.0.check();
        }
    }
}

impl Sched {
    /// Panics unless the lock's invariants hold: entries in epoch order, a runnable entry waiting on
    /// no parent, an unreleased obligation holding a token, and a handover only while no run is in flight.
    #[cfg(debug_assertions)]
    fn check(&self) {
        let mut previous: Option<EpochId> = None;
        for (id, entry) in self.entries.iter() {
            assert!(previous.is_none_or(|before: EpochId| before < *id), "lithia: entries out of epoch order");
            assert_eq!(*id, entry.epoch.id, "lithia: an entry filed under another epoch");
            assert!(
                !entry.due.contains(Due::RUNNABLE) || entry.waiting.is_empty(),
                "lithia: a runnable entry still waits on a parent"
            );
            previous = Some(*id);
        }
        let served: &[Served] = self.run.as_ref().map_or(&[], |slot: &RunSlot| slot.served.as_slice());
        for record in served.iter().chain(&self.handover) {
            assert!(record.released || record.tokens > 0, "lithia: an unreleased obligation holds no token");
        }
        assert!(self.run.is_none() || self.handover.is_empty(), "lithia: a handover beside a run in flight");
    }

    pub(crate) fn new(unseeded: bool) -> Sched {
        Sched {
            entries: Entries::default(),
            run: None,
            handover: ServedList::new(),
            queued: false,
            unseeded,
            ran_at: Stamp::NONE,
            subscribers: Subscribers::new(),
            trace: crate::testing::trace::Trace::new(),
            generation: Vec::new(),
            keyed: KeyedSet::default(),
            holdover: KeyedSet::default(),
            closed: false,
            releasing: false,
            failed_by_fragility: false,
            gated: Vec::new(),
            suppress_in: Vec::new(),
            whole_deferred: 0,
            rate: crate::diag::RateWindow::new(),
        }
    }

    /// Whether this node, as `child`'s creator, may still replace it or keep it: an entry it has not
    /// decided, a run it will start or restart, or a run under way that has not reclaimed `child`.
    pub(crate) fn decides(&self, child: &Header) -> bool {
        let undecided_entry: bool = self.entries.values().any(|entry: &Entry| !entry.due.contains(Due::RUNNABLE));
        let next_run: bool = self.run.is_none()
            && (self.queued
                || !self.handover.is_empty()
                || self.entries.values().any(|entry: &Entry| entry.due.contains(Due::RUNNABLE)));
        let restart: bool = self.run.as_ref().is_some_and(|slot: &RunSlot| slot.restart);
        undecided_entry || next_run || restart || child.keyed.undecided.load(Ordering::SeqCst)
    }

    /// Releases the generation: unkeyed nodes into `out`, keyed ones into the holdover.
    pub(crate) fn take_generation(&mut self, out: &mut Vec<NodeRef>) {
        // Undecided until the release lands: a run of theirs must not start in between.
        for node in &self.generation {
            node.header().keyed.undecided.store(true, Ordering::SeqCst);
        }
        out.append(&mut self.generation);
        let keyed: KeyedSet = std::mem::take(&mut self.keyed);
        for node in keyed.iter() {
            node.header().keyed.undecided.store(true, Ordering::SeqCst);
        }
        self.holdover.merge(keyed);
    }

    /// Releases everything the node's runs hold: generation, keyed nodes and holdover.
    pub(crate) fn release_all(&mut self) -> Vec<NodeRef> {
        let mut out: Vec<NodeRef> = std::mem::take(&mut self.generation);
        self.keyed.drain_into(&mut out);
        self.holdover.drain_into(&mut out);
        out
    }

    /// Whether the node's runs hold any node.
    pub(crate) fn holds_creations(&self) -> bool {
        !self.generation.is_empty() || !self.keyed.is_empty() || !self.holdover.is_empty()
    }

    /// Every node the node's runs hold.
    pub(crate) fn creations(&self) -> impl Iterator<Item = &NodeRef> {
        self.generation.iter().chain(self.keyed.iter()).chain(self.holdover.iter())
    }

    /// Removes one held node, wherever it is held.
    pub(crate) fn remove_creation(&mut self, id: NodeId) -> Option<NodeRef> {
        if let Some(at) = self.generation.iter().position(|member: &NodeRef| member.header().id == id) {
            return Some(self.generation.swap_remove(at));
        }
        self.keyed.remove(id).or_else(|| self.holdover.remove(id))
    }

    /// The unreleased record a run, or the handover to the next run, holds for `epoch`.
    pub(crate) fn owed_mut(&mut self, epoch: EpochId) -> Option<&mut Served> {
        if let Some(slot) = self.run.as_mut()
            && let Some(served) = slot.served.iter_mut().find(|s: &&mut Served| s.epoch.id == epoch && !s.released)
        {
            return Some(served);
        }
        self.handover.iter_mut().find(|s: &&mut Served| s.epoch.id == epoch && !s.released)
    }

    /// The live subscribers in id order, pruning freed ones; the caller drops them after unlocking.
    pub(crate) fn children(&mut self) -> Children {
        self.subscribers.live()
    }

    /// Whether the node still owes its children a release for `epoch` through a run.
    pub(crate) fn owes(&self, epoch: EpochId) -> bool {
        let owed = |s: &Served| s.epoch.id == epoch && !s.released;
        self.run.as_ref().is_some_and(|slot: &RunSlot| slot.served.iter().any(owed)) || self.handover.iter().any(owed)
    }

    /// Whether any run work is pending or in flight.
    pub(crate) fn busy(&self) -> bool {
        self.run.is_some() || self.queued || !self.handover.is_empty() || self.releasing
    }

    /// No run work pending or in flight, no first run still to seed, and no entry open.
    pub(crate) fn is_quiet(&self) -> bool {
        !(self.busy() || self.unseeded || !self.entries.is_empty())
    }

    /// Closes the node to new children; returns its generation and the children linked so far.
    pub(crate) fn close(&mut self) -> (Vec<NodeRef>, Vec<NodeRef>) {
        self.closed = true;
        let linked: Vec<NodeRef> = self.subscribers.live().into_vec();
        (self.release_all(), linked)
    }

    /// Whether any node subscribes to this one.
    pub(crate) fn has_subscribers(&self) -> bool {
        !self.subscribers.is_empty()
    }

    /// The entry for `epoch`, opened with one of its tokens if there is none; with the children to frame
    /// in it when this opened it and no run still owes them the epoch.
    pub(crate) fn open_entry(&mut self, epoch: &Arc<Epoch>) -> (&mut Entry, Option<Children>) {
        let opened: bool = !self.entries.contains_key(&epoch.id);
        let framed: Option<Children> = (opened && !self.owes(epoch.id)).then(|| self.children());
        if opened {
            epoch.open_token();
        }
        (self.entries.get_or_insert_with(epoch.id, || Entry::new(epoch.clone())), framed)
    }

    /// No run in flight and an entry ready to run: a run that is owed already.
    pub(crate) fn owes_a_run(&self) -> bool {
        self.run.is_none() && self.entries.values().any(Entry::is_ready)
    }

    /// Records a node created by this node's run in its generation, or, keyed, in its keyed set;
    /// returns the keyed nodes created before whose key hashes alike, to check for a duplicate.
    pub(crate) fn note_created(&mut self, node: &NodeRef, keyed: bool) -> Vec<NodeRef> {
        crate::testing::trace::note_created(&mut self.trace, node);
        if self.closed {
            return Vec::new();
        }
        if keyed {
            crate::expand::keyed::note_fresh(self, node)
        } else {
            self.generation.push(node.clone());
            Vec::new()
        }
    }
}

/// A node's open entries in ascending epoch order; most nodes hold none or one at a time.
#[derive(Default)]
pub(crate) struct Entries(Vec<(EpochId, Entry)>);

impl Entries {
    fn position(&self, epoch: EpochId) -> Result<usize, usize> {
        self.0.binary_search_by_key(&epoch, |(id, _): &(EpochId, Entry)| *id)
    }

    pub(crate) fn get(&self, epoch: &EpochId) -> Option<&Entry> {
        self.position(*epoch).ok().map(|at: usize| &self.0[at].1)
    }

    pub(crate) fn get_mut(&mut self, epoch: &EpochId) -> Option<&mut Entry> {
        self.position(*epoch).ok().map(|at: usize| &mut self.0[at].1)
    }

    pub(crate) fn contains_key(&self, epoch: &EpochId) -> bool {
        self.position(*epoch).is_ok()
    }

    /// The entry for `epoch`, made by `make` if there is none.
    pub(crate) fn get_or_insert_with(&mut self, epoch: EpochId, make: impl FnOnce() -> Entry) -> &mut Entry {
        let at: usize = match self.position(epoch) {
            Ok(at) => at,
            Err(at) => {
                self.0.insert(at, (epoch, make()));
                at
            }
        };
        &mut self.0[at].1
    }

    pub(crate) fn remove(&mut self, epoch: &EpochId) -> Option<Entry> {
        let at: usize = self.position(*epoch).ok()?;
        let (_, entry): (EpochId, Entry) = self.0.remove(at);
        self.shrink();
        Some(entry)
    }

    /// Removes, in epoch order, every entry `accept` takes, handing each to `take`.
    pub(crate) fn take_where(&mut self, mut accept: impl FnMut(&Entry) -> bool, mut take: impl FnMut(Entry)) {
        for (_, entry) in self.0.extract_if(.., |(_, entry): &mut (EpochId, Entry)| accept(entry)) {
            take(entry);
        }
        self.shrink();
    }

    /// Lets go of a buffer an epoch backlog grew, once it is empty again.
    fn shrink(&mut self) {
        if self.0.is_empty() && self.0.capacity() > 8 {
            self.0 = Vec::new();
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&EpochId, &Entry)> {
        self.0.iter().map(|(id, entry): &(EpochId, Entry)| (id, entry))
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = &EpochId> {
        self.0.iter().map(|(id, _): &(EpochId, Entry)| id)
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &Entry> {
        self.0.iter().map(|(_, entry): &(EpochId, Entry)| entry)
    }
}

impl IntoIterator for Entries {
    type Item = (EpochId, Entry);
    type IntoIter = std::vec::IntoIter<(EpochId, Entry)>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// What a node tracks for one epoch it is framed in (spec §7.2).
pub(crate) struct Entry {
    pub(crate) epoch: Arc<Epoch>,
    /// Parents not yet released.
    pub(crate) waiting: ParentMask,
    /// Writers on the chains that reached this entry.
    pub(crate) writers: WriterSet,
    /// Newest change stamp a release carried, or none.
    pub(crate) newest: Stamp,
    /// Change stamp to hand on to the children when this entry settles.
    pub(crate) carry: Stamp,
    /// What makes the node due in this epoch besides a newer change, and whether it is runnable.
    pub(crate) due: Due,
    /// The written node at the head of the chain, for cut diagnostics.
    pub(crate) origin: Option<&'static Site>,
}

bitflags::bitflags! {
    /// Why an entry's node runs in its epoch besides a newer change, and how far deciding it got.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub(crate) struct Due: u8 {
        /// The node's first run is due in this epoch.
        const FIRST = 1;
        /// A `lithia::reflow` is due in this epoch.
        const REFLOW = 1 << 1;
        /// Run whatever the versions say (fragility recovery, ghost parent).
        const FORCE = 1 << 2;
        /// The writer check refused the node in this epoch.
        const CUT = 1 << 3;
        /// Ready to be taken by the next run start.
        const RUNNABLE = 1 << 4;
    }
}

impl Entry {
    pub(crate) fn new(epoch: Arc<Epoch>) -> Entry {
        Entry {
            epoch,
            waiting: ParentMask::new(),
            writers: WriterSet::new(),
            newest: Stamp::NONE,
            carry: Stamp::NONE,
            due: Due::empty(),
            origin: None,
        }
    }

    /// Runnable and waiting on no parent: the next run start takes it.
    pub(crate) fn is_ready(&self) -> bool {
        self.due.contains(Due::RUNNABLE) && self.waiting.is_empty()
    }

    /// Due only for a reflow: no first run, no forced rerun, no change newer than `ran_at`.
    pub(crate) fn reflow_only(&self, ran_at: Stamp) -> bool {
        self.due.intersection(Due::FIRST | Due::REFLOW | Due::FORCE) == Due::REFLOW && self.newest <= ran_at
    }
}

/// One epoch a run serves.
pub(crate) struct Served {
    pub(crate) epoch: Arc<Epoch>,
    pub(crate) writers: WriterSet,
    /// Open-entry tokens held in `epoch`, closed when the run ends or suspends.
    pub(crate) tokens: u32,
    /// Children were released for `epoch`.
    pub(crate) released: bool,
    /// Change stamp to hand on to the children at release.
    pub(crate) carry: Stamp,
}

/// The run in flight on a node.
pub(crate) struct RunSlot {
    pub(crate) id: RunId,
    pub(crate) shared: Arc<RunControl>,
    pub(crate) served: ServedList,
    /// Superseded: start a new run once this one has stopped.
    pub(crate) restart: bool,
    /// It protected a superseded run and has produced no item yet: new input waits for its first
    /// item instead of superseding it, so a node always makes progress (spec §7.3 item 4).
    pub(crate) protected: bool,
}

/// Merges `incoming` into `list`, keeping one record per epoch.
/// The epochs a run or a handover serves; mostly one, inline.
pub(crate) type ServedList = smallvec::SmallVec<[Served; 1]>;

pub(crate) fn merge_served(list: &mut ServedList, incoming: Served) {
    match list.iter_mut().find(|s: &&mut Served| s.epoch.id == incoming.epoch.id) {
        Some(existing) => {
            existing.tokens += incoming.tokens;
            existing.released = existing.released && incoming.released;
            existing.writers.union(&incoming.writers);
            existing.carry = existing.carry.max(incoming.carry);
        }
        None => list.push(incoming),
    }
}

/// Most subscribers kept inline before they spill into a map.
const SPILL: usize = 32;

/// A node's subscribers by id, held weakly and kept in id order: a sorted vector while few, a map
/// once many, so a hub's thousands of children leave in logarithmic time.
pub(crate) enum Subscribers {
    Few(smallvec::SmallVec<[(NodeId, WeakNodeRef); 2]>),
    Many(BTreeMap<NodeId, WeakNodeRef>),
}

impl Subscribers {
    pub(crate) const fn new() -> Subscribers {
        Subscribers::Few(smallvec::SmallVec::new_const())
    }

    pub(crate) fn insert(&mut self, id: NodeId, child: WeakNodeRef) {
        match self {
            Subscribers::Few(few) => match few.binary_search_by_key(&id, |(known, _): &(NodeId, WeakNodeRef)| *known) {
                Ok(at) => few[at].1 = child,
                Err(_) if few.len() >= SPILL => {
                    let mut many: BTreeMap<NodeId, WeakNodeRef> = few.drain(..).collect();
                    many.insert(id, child);
                    *self = Subscribers::Many(many);
                }
                Err(at) => few.insert(at, (id, child)),
            },
            Subscribers::Many(many) => {
                many.insert(id, child);
            }
        }
    }

    /// Removes `id`; true if it was there.
    pub(crate) fn remove(&mut self, id: NodeId) -> bool {
        match self {
            Subscribers::Few(few) => match few.binary_search_by_key(&id, |(known, _): &(NodeId, WeakNodeRef)| *known) {
                Ok(at) => {
                    few.remove(at);
                    true
                }
                Err(_) => false,
            },
            Subscribers::Many(many) => many.remove(&id).is_some(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        match self {
            Subscribers::Few(few) => few.is_empty(),
            Subscribers::Many(many) => many.is_empty(),
        }
    }

    pub(crate) fn clear(&mut self) {
        *self = Subscribers::new();
    }

    /// The live subscribers in id order; freed ones are pruned.
    pub(crate) fn live(&mut self) -> Children {
        let mut out: Children = Children::new();
        let mut keep = |_: &NodeId, child: &mut WeakNodeRef| -> bool {
            match child.upgrade() {
                Some(node) => {
                    out.push(node);
                    true
                }
                None => false,
            }
        };
        match self {
            Subscribers::Few(few) => few.retain(|(id, child): &mut (NodeId, WeakNodeRef)| keep(id, child)),
            Subscribers::Many(many) => many.retain(|id: &NodeId, child: &mut WeakNodeRef| keep(id, child)),
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dangling() -> WeakNodeRef {
        let weak: std::sync::Weak<crate::graph::node::Node<u32>> = std::sync::Weak::new();
        weak
    }

    #[test]
    fn subscribers_spill_into_a_map_and_keep_every_id() {
        let mut subscribers: Subscribers = Subscribers::new();
        let ids: Vec<NodeId> = (0..2 * SPILL).map(|_: usize| crate::graph::ids::next_node_id()).collect();
        // Out of order, so the inline part sorts and the spill carries them over.
        for id in ids.iter().rev() {
            subscribers.insert(*id, dangling());
        }
        assert!(matches!(subscribers, Subscribers::Many(_)));
        assert!(ids.iter().step_by(2).all(|id: &NodeId| subscribers.remove(*id)));
        assert!(!subscribers.remove(ids[0]), "removed twice");
        assert!(!subscribers.is_empty());
        // Freed children are pruned as the live ones are taken.
        assert!(subscribers.live().is_empty());
        assert!(subscribers.is_empty());
    }

    #[test]
    fn few_subscribers_stay_in_id_order() {
        let mut subscribers: Subscribers = Subscribers::new();
        let (a, b, c): (NodeId, NodeId, NodeId) =
            (crate::graph::ids::next_node_id(), crate::graph::ids::next_node_id(), crate::graph::ids::next_node_id());
        for id in [c, a, b] {
            subscribers.insert(id, dangling());
        }
        let Subscribers::Few(few) = &subscribers else { panic!("three subscribers spilled") };
        assert_eq!(few.iter().map(|(id, _): &(NodeId, WeakNodeRef)| *id).collect::<Vec<NodeId>>(), vec![a, b, c]);
        assert!(subscribers.remove(b) && !subscribers.remove(b));
    }
}
