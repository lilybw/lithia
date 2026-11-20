// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! The node record behind every handle, and its scheduler state (spec §5).

use std::any::TypeId;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};

use crate::sync::{ArcCell, AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Mutex, MutexGuard};
use tokio::runtime::Handle;
use tokio::sync::watch;

use crate::epoch::{Epoch, Scope};
use crate::ids::{EpochId, NodeId};
use crate::keyed::{Inputs, Key, KeyedSet};
use crate::options::{Equals, Flags, Fragility, GhostPolicy};
use crate::run::{RunShared, Yielder};
use crate::sets::{ParentMask, WriterSet};
use crate::{Failure, NodeValue, Site, Status};

/// Strong, type-erased reference to a node.
pub(crate) type NodeRef = Arc<dyn AnyNode>;
/// Weak, type-erased reference to a node.
pub(crate) type WeakNodeRef = Weak<dyn AnyNode>;

/// Bits of a node's phase word; `Status` derives from it (spec §5.3).
pub(crate) mod phase {
    /// A run is in flight and has produced no item yet.
    pub(crate) const PENDING: u32 = 1;
    /// The last run panicked, fragility failed the node, or its runtime shut down.
    pub(crate) const FAILED: u32 = 2;
    /// The node was disposed.
    pub(crate) const GHOST: u32 = 4;
}

/// What created the node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `effect!(value)`: written directly, no runs of its own.
    Source,
    /// `effect!(async { … })`: a source fed by one async run.
    AsyncSource,
    /// `effect!(|deps| …)`: runs a body on its parents.
    Derived,
}

/// Boxed future of one run.
pub(crate) type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// The code a node runs.
pub(crate) enum Body<K: NodeValue> {
    /// A synchronous body producing one item.
    Sync(Arc<dyn Fn() -> K + Send + Sync>),
    /// An async body producing one item.
    Async(Arc<dyn Fn() -> BoxFut<K> + Send + Sync>),
    /// A generator body producing items through its yielder.
    Gen(Arc<dyn Fn(Yielder<K>) -> BoxFut<()> + Send + Sync>),
    /// An async source block, run once.
    Once(BoxFut<K>),
    /// An async source block that yields, run once.
    OnceGen(Box<dyn FnOnce(Yielder<K>) -> BoxFut<()> + Send>),
}

/// Rerun counting for the rerun-rate warning (spec §12).
pub(crate) struct RateWindow {
    pub(crate) start_ms: u64,
    pub(crate) count: u64,
    pub(crate) warned: bool,
}

/// Type-independent part of a node.
pub(crate) struct Header {
    pub(crate) id: NodeId,
    pub(crate) site: &'static Site,
    pub(crate) creator: Option<NodeId>,
    pub(crate) kind: Kind,
    /// Whether the value type is `()`, i.e. the node is an effect.
    pub(crate) is_unit: bool,
    pub(crate) version: AtomicU64,
    pub(crate) phase: AtomicU32,
    pub(crate) failure: ArcCell<Failure>,
    /// Id of the current run; stale items are discarded.
    pub(crate) runs: AtomicU64,
    /// Serializes writers; never poisons.
    pub(crate) write: Mutex<()>,
    /// The node's scheduling lock (spec §11.4 step 1).
    pub(crate) sched: Mutex<Sched>,
    /// Strong references to the parents; taken on disposal.
    pub(crate) parents: Mutex<Option<Vec<NodeRef>>>,
    /// Parent ids, sorted; a parent's index is its position here.
    pub(crate) parent_ids: Box<[NodeId]>,
    pub(crate) fragility: Option<Fragility>,
    pub(crate) ghost: GhostPolicy,
    pub(crate) flags: Flags,
    pub(crate) runtime: Option<Handle>,
    /// Whether its body is synchronous, so its runs may take the fast path (spec §11.4, step 4).
    pub(crate) sync_body: bool,
    pub(crate) scope: Arc<Scope>,
    /// Whether the node still counts towards its scope's live nodes.
    pub(crate) counted: AtomicBool,
    /// The `changes()` channel, made by the first stream.
    pub(crate) changes: Mutex<Option<watch::Sender<u64>>>,
    pub(crate) rate: Mutex<RateWindow>,
    /// Outside handles that anchor the node.
    pub(crate) anchors: AtomicUsize,
    /// Handles that bodies captured; readers for the unread-memo warning.
    pub(crate) captured: AtomicUsize,
    /// Self-reference while support holds the node.
    pub(crate) support: Mutex<Option<NodeRef>>,
    /// Set by `.scoped()`: only the node's own handles support it.
    pub(crate) scoped: AtomicBool,
    /// The node whose run's generation holds this node.
    pub(crate) held_by: Mutex<Option<WeakNodeRef>>,
    /// Nodes this node's body may write.
    pub(crate) targets: Box<[WeakNodeRef]>,
    /// Nodes whose bodies may write this node.
    pub(crate) writers: Mutex<Vec<WeakNodeRef>>,
    /// Warn if freed before its first run got under way.
    pub(crate) warn_first_run: bool,
    /// The first run finished, yielded an item or was polled a second time.
    pub(crate) under_way: AtomicBool,
    /// The value type, for reclaiming a keyed node.
    pub(crate) value_type: TypeId,
    /// The `key` option's value; set at creation, never changed.
    pub(crate) key: Option<Key>,
    /// What a keyed node's body last captured, to compare a reclaiming creation against.
    pub(crate) inputs: Mutex<Option<Inputs>>,
    /// The run id an async source block may run under; a write or a new block moves `runs` past it.
    pub(crate) block_run: AtomicU64,
    /// The identity it writes under in writer sets: its id, or for a node created in a run, its
    /// lineage, which the same creation in the creator's next run shares (spec §7.4).
    pub(crate) writer: NodeId,
    /// Whether support holds the node while an anchor reaches it: a top-level node with parents.
    pub(crate) supportable: bool,
    /// A node its creator has yet to finish deciding: being let go of, in the holdover, or being
    /// reclaimed until its refresh is done (spec §7.2, Creator order).
    pub(crate) undecided: AtomicBool,
    /// The lease of an async source block's `move` captures, ended once the block stops for good.
    pub(crate) lease: Mutex<Option<Arc<crate::capture::Lease>>>,
}

/// Scheduler state of one node, guarded by its scheduling lock.
pub(crate) struct Sched {
    /// Open entries not taken by a run, one per epoch the node is framed in.
    pub(crate) entries: Entries,
    /// The run in flight, if any.
    pub(crate) run: Option<RunSlot>,
    /// Epochs a superseded run served, handed to the next run.
    pub(crate) inherited: Vec<Served>,
    /// A run task was spawned and has not started yet.
    pub(crate) queued: bool,
    /// Created, but its creation has not been delivered yet.
    pub(crate) unseeded: bool,
    /// Clock stamp when the last run started.
    pub(crate) ran_at: u64,
    /// Children by id, held weakly; a freed child removes itself, without a scan of the others.
    pub(crate) subscribers: BTreeMap<NodeId, WeakNodeRef>,
    /// Nodes created by this node's runs, held weakly, for `lithia::test::settled`.
    pub(crate) created: Vec<WeakNodeRef>,
    /// Length of `created` after its last prune; it is pruned again once twice as long.
    pub(crate) created_floor: usize,
    /// Nodes its runs wrote or reflowed without an edge, held weakly, for `lithia::test::settled`.
    pub(crate) wrote: Vec<WeakNodeRef>,
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
    pub(crate) suppress_in: Vec<Arc<Epoch>>,
}

impl Sched {
    pub(crate) fn new(unseeded: bool) -> Sched {
        Sched {
            entries: Entries::default(),
            run: None,
            inherited: Vec::new(),
            queued: false,
            unseeded,
            ran_at: 0,
            subscribers: BTreeMap::new(),
            created: Vec::new(),
            created_floor: 0,
            wrote: Vec::new(),
            generation: Vec::new(),
            keyed: KeyedSet::default(),
            holdover: KeyedSet::default(),
            closed: false,
            releasing: false,
            failed_by_fragility: false,
            gated: Vec::new(),
            suppress_in: Vec::new(),
        }
    }

    /// Whether this node, as `child`'s creator, may still replace it or keep it: an entry it has not
    /// decided, a run it will start or restart, or a run under way that has not reclaimed `child`.
    pub(crate) fn decides(&self, child: &Header) -> bool {
        let undecided_entry: bool = self.entries.values().any(|entry: &Entry| !entry.runnable);
        let next_run: bool = self.run.is_none()
            && (self.queued || !self.inherited.is_empty() || self.entries.values().any(|entry: &Entry| entry.runnable));
        let restart: bool = self.run.as_ref().is_some_and(|slot: &RunSlot| slot.restart);
        undecided_entry || next_run || restart || child.undecided.load(Ordering::SeqCst)
    }

    /// Releases the generation: unkeyed nodes into `out`, keyed ones into the holdover.
    pub(crate) fn release_generation(&mut self, out: &mut Vec<NodeRef>) {
        // Undecided until the release lands: a run of theirs must not start in between.
        for node in &self.generation {
            node.header().undecided.store(true, Ordering::SeqCst);
        }
        out.append(&mut self.generation);
        let keyed: KeyedSet = std::mem::take(&mut self.keyed);
        for node in keyed.iter() {
            node.header().undecided.store(true, Ordering::SeqCst);
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
        self.inherited.iter_mut().find(|s: &&mut Served| s.epoch.id == epoch && !s.released)
    }

    /// The live subscribers in id order, pruning freed ones; the caller drops them after unlocking.
    pub(crate) fn children(&mut self) -> Vec<NodeRef> {
        let mut out: Vec<NodeRef> = Vec::with_capacity(self.subscribers.len());
        self.subscribers.retain(|_: &NodeId, weak: &mut WeakNodeRef| match weak.upgrade() {
            Some(node) => {
                out.push(node);
                true
            }
            None => false,
        });
        out
    }

    /// Whether the node still owes its children a release for `epoch` through a run.
    pub(crate) fn owes(&mut self, epoch: EpochId) -> bool {
        self.owed_mut(epoch).is_some()
    }

    /// Whether any run work is pending or in flight.
    pub(crate) fn busy(&self) -> bool {
        self.run.is_some() || self.queued || !self.inherited.is_empty() || self.releasing
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

    /// Inserts or replaces the entry for `epoch`.
    pub(crate) fn insert(&mut self, epoch: EpochId, entry: Entry) {
        match self.position(epoch) {
            Ok(at) => self.0[at].1 = entry,
            Err(at) => self.0.insert(at, (epoch, entry)),
        }
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
    /// Newest change stamp a release carried, or 0.
    pub(crate) newest: u64,
    /// Change stamp to hand on to the children when this entry settles.
    pub(crate) carry: u64,
    /// The node's first run is due in this epoch.
    pub(crate) first: bool,
    /// A `lithia::reflow` is due in this epoch.
    pub(crate) reflow: bool,
    /// Run whatever the versions say (fragility recovery, ghost parent).
    pub(crate) force: bool,
    /// The writer check refused the node in this epoch.
    pub(crate) cut: bool,
    /// Ready to be taken by the next run start.
    pub(crate) runnable: bool,
    /// The written node at the head of the chain, for cut diagnostics.
    pub(crate) origin: Option<&'static Site>,
}

impl Entry {
    pub(crate) fn new(epoch: Arc<Epoch>) -> Entry {
        Entry {
            epoch,
            waiting: ParentMask::new(),
            writers: WriterSet::new(),
            newest: 0,
            carry: 0,
            first: false,
            reflow: false,
            force: false,
            cut: false,
            runnable: false,
            origin: None,
        }
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
    pub(crate) carry: u64,
}

/// The run in flight on a node.
pub(crate) struct RunSlot {
    pub(crate) id: u64,
    pub(crate) shared: Arc<RunShared>,
    pub(crate) served: Vec<Served>,
    /// Superseded: start a new run once this one has stopped.
    pub(crate) restart: bool,
    /// It replaced a superseded run and has produced no item yet: new input waits for its first
    /// item instead of superseding it, so a node always makes progress (spec §7.3 item 4).
    pub(crate) replaced: bool,
}

/// Merges `incoming` into `list`, keeping one record per epoch.
pub(crate) fn merge_served(list: &mut Vec<Served>, incoming: Served) {
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

impl Header {
    pub(crate) fn phase(&self) -> u32 {
        self.phase.load(Ordering::SeqCst)
    }

    pub(crate) fn is_ghost(&self) -> bool {
        self.phase() & phase::GHOST != 0
    }

    pub(crate) fn set_bits(&self, bits: u32) {
        self.phase.fetch_or(bits, Ordering::SeqCst);
    }

    pub(crate) fn clear_bits(&self, bits: u32) {
        self.phase.fetch_and(!bits, Ordering::SeqCst);
    }

    /// Condition of the node as seen by readers (spec §5.3).
    pub(crate) fn status(&self) -> Status {
        let bits: u32 = self.phase();
        if bits & phase::GHOST != 0 {
            Status::Ghost
        } else if bits & phase::FAILED != 0 {
            match self.failure.load_full() {
                Some(failure) => Status::Failed(failure),
                None => Status::Failed(Arc::new(Failure {
                    message: "failed".to_string(),
                    site: *self.site,
                    causes: Vec::new(),
                })),
            }
        } else if bits & phase::PENDING != 0 {
            Status::Pending
        } else {
            Status::Ready
        }
    }

    /// Enters `Pending` for a starting run and leaves `Failed` in one step; clears a failure record.
    pub(crate) fn begin_run(&self) {
        let update = |bits: u32| Some((bits | phase::PENDING) & !phase::FAILED);
        let before: u32 = match self.phase.fetch_update(Ordering::SeqCst, Ordering::SeqCst, update) {
            Ok(bits) | Err(bits) => bits,
        };
        if before & phase::FAILED != 0 {
            self.failure.store(None);
        }
    }

    /// Leaves `Pending` and `Failed`, as a write does; clears a failure record only if there is one.
    pub(crate) fn clear_pending_failed(&self) {
        if self.phase.fetch_and(!(phase::PENDING | phase::FAILED), Ordering::SeqCst) & phase::FAILED != 0 {
            self.failure.store(None);
        }
    }

    /// Records a failure and enters `Failed`.
    pub(crate) fn fail(&self, failure: Failure) {
        self.failure.store(Some(Arc::new(failure)));
        self.set_bits(phase::FAILED);
        self.clear_bits(phase::PENDING);
    }

    /// Index of `parent` in this node's parent list.
    pub(crate) fn parent_index(&self, parent: NodeId) -> Option<usize> {
        self.parent_ids.binary_search(&parent).ok()
    }

    /// Strong references to the live children, pruning freed ones.
    pub(crate) fn subscribers(&self) -> Vec<NodeRef> {
        let children: Vec<NodeRef> = self.sched.lock().children();
        children
    }

    /// Removes a freed or disposed child from the subscriber list; the last one leaving may leave
    /// a memo unread (spec §12).
    pub(crate) fn unsubscribe(&self, child: NodeId) {
        let mut guard: MutexGuard<'_, Sched> = self.sched.lock();
        let last: bool = guard.subscribers.remove(&child).is_some() && guard.subscribers.is_empty();
        drop(guard);
        if last {
            crate::support::check_unread(self);
        }
    }

    /// Notifies `changes()` streams of a new value.
    pub(crate) fn notify_changes(&self) {
        if let Some(sender) = self.changes.lock().as_ref() {
            sender.send_modify(|seq: &mut u64| *seq = seq.wrapping_add(1));
        }
    }
}

/// Internal record of one node; type-erased behind every handle.
pub(crate) struct Node<K: NodeValue> {
    pub(crate) header: Header,
    /// Lock-free reads; emptied only by disposal under `ON_GHOST_CAUSE_FAILURE`.
    pub(crate) value: ArcCell<K>,
    /// Empty for source nodes; taken on disposal.
    pub(crate) body: Mutex<Option<Body<K>>>,
    /// Empty without `equals`; a reclaiming creation replaces it (spec §10.1, Keyed nodes).
    pub(crate) equals: Option<Mutex<Equals<K>>>,
}

impl<K: NodeValue> Node<K> {
    /// The current comparator, if the node has one.
    pub(crate) fn comparator(&self) -> Option<Equals<K>> {
        self.equals.as_ref().map(|cell: &Mutex<Equals<K>>| cell.lock().clone())
    }

    /// Takes a reclaiming creation's comparator; returns the one it replaced.
    pub(crate) fn adopt_equals(&self, equals: Option<Equals<K>>) -> Option<Equals<K>> {
        let (Some(cell), Some(equals)) = (&self.equals, equals) else {
            return None;
        };
        Some(std::mem::replace(&mut *cell.lock(), equals))
    }
}

/// Operations the scheduler needs on a node of any value type.
pub(crate) trait AnyNode: Send + Sync + 'static {
    fn header(&self) -> &Header;
    /// Spawns a run task on the node's runtime.
    fn spawn_run(self: Arc<Self>);
    /// The node's next run, to poll on this thread or to spawn (spec §11.4, step 4).
    fn make_run(self: Arc<Self>) -> crate::inline::RunTask;
    /// Drops the value where the ghost policy says so, and the body.
    fn release_payload(&self);
    /// The node as `Any`, to recover its value type.
    fn into_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync>;
}

impl<K: NodeValue> AnyNode for Node<K> {
    fn header(&self) -> &Header {
        &self.header
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
        self
    }

    fn spawn_run(self: Arc<Self>) {
        let runtime: Option<Handle> = self.header.runtime.clone();
        let future: crate::run::RunFuture<K> = crate::run::RunFuture::new(Arc::downgrade(&self));
        drop(self);
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
            None => drop(future),
        }
    }

    fn make_run(self: Arc<Self>) -> crate::inline::RunTask {
        let future: crate::run::RunFuture<K> = crate::run::RunFuture::new(Arc::downgrade(&self));
        drop(self);
        Box::pin(future)
    }

    fn release_payload(&self) {
        if self.header.ghost == GhostPolicy::CauseFailure
            && let Some(value) = self.value.swap(None)
        {
            crate::drops::drop_value(value, self.header.runtime.as_ref());
        }
        let body: Option<Body<K>> = self.body.lock().take();
        if let Some(body) = body {
            crate::drops::drop_later(body, self.header.runtime.as_ref());
        }
    }
}

impl<K: NodeValue> Node<K> {
    pub(crate) fn is_unit() -> bool {
        TypeId::of::<K>() == TypeId::of::<()>()
    }
}

impl<K: NodeValue> Drop for Node<K> {
    fn drop(&mut self) {
        let _op: crate::support::OpGuard = crate::support::enter();
        let header: &mut Header = &mut self.header;
        let ghost: bool = header.phase.load(Ordering::SeqCst) & phase::GHOST != 0;
        if header.warn_first_run && !ghost && !header.under_way.load(Ordering::SeqCst) {
            crate::diag::freed_before_first_run(header.site);
        }
        if header.counted.swap(false, Ordering::SeqCst) {
            header.scope.live.fetch_sub(1, Ordering::SeqCst);
        }
        let sched: &mut Sched = header.sched.get_mut();
        let mut tokens: Vec<Arc<Epoch>> = Vec::new();
        for (_, entry) in std::mem::take(&mut sched.entries) {
            tokens.push(entry.epoch);
        }
        if let Some(slot) = sched.run.take() {
            // Woken, a parked run finds its node gone and drops its future, captures included.
            slot.shared.cancel();
            slot.shared.wake();
            for served in slot.served {
                for _ in 0..served.tokens {
                    tokens.push(served.epoch.clone());
                }
            }
        }
        for served in std::mem::take(&mut sched.inherited) {
            for _ in 0..served.tokens {
                tokens.push(served.epoch.clone());
            }
        }
        sched.subscribers.clear();
        sched.created.clear();
        let generation: Vec<NodeRef> = sched.release_all();
        let parents: Option<Vec<NodeRef>> = header.parents.get_mut().take();
        if let Some(list) = parents.as_ref() {
            for parent in list {
                parent.header().unsubscribe(header.id);
            }
        }
        for epoch in tokens {
            epoch.close_token();
        }
        let runtime: Option<Handle> = header.runtime.clone();
        if let Some(value) = self.value.swap(None) {
            crate::drops::drop_value(value, runtime.as_ref());
        }
        if let Some(body) = self.body.get_mut().take() {
            crate::drops::drop_later(body, runtime.as_ref());
        }
        // Handles its block moved out count before its targets lose its support.
        let lease: Option<Arc<crate::capture::Lease>> = self.header.lease.lock().take();
        if let Some(lease) = lease {
            lease.end();
        }
        // A freed writer no longer supports its targets; its generation loses its creation edges.
        for target in self.header.targets.iter().filter_map(WeakNodeRef::upgrade) {
            crate::support::start(target);
        }
        crate::support::release_generation(generation);
        if let Some(list) = parents {
            crate::drops::release_parents(list);
        }
        crate::settle_wait::bump();
    }
}
