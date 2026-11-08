//! The node record behind every handle, and its scheduler state (spec §5).

use std::any::TypeId;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};

use crate::sync::{ArcCell, AtomicBool, AtomicU32, AtomicU64, Mutex};
use tokio::runtime::Handle;
use tokio::sync::watch;

use crate::epoch::{Epoch, Scope};
use crate::ids::{EpochId, NodeId};
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
    pub(crate) scope: Arc<Scope>,
    /// Whether the node still counts towards its scope's live nodes.
    pub(crate) counted: AtomicBool,
    pub(crate) changes: Mutex<Option<watch::Sender<u64>>>,
    pub(crate) first_run_done: AtomicBool,
    pub(crate) rate: Mutex<RateWindow>,
}

/// Scheduler state of one node, guarded by its scheduling lock.
pub(crate) struct Sched {
    /// Open entries not taken by a run, one per epoch the node is framed in.
    pub(crate) entries: BTreeMap<EpochId, Entry>,
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
    /// Children, held weakly; a freed child removes itself.
    pub(crate) subscribers: Vec<(NodeId, WeakNodeRef)>,
    /// Nodes created by this node's runs, held weakly, for `lithia::test::settled`.
    pub(crate) created: Vec<WeakNodeRef>,
    /// Failed through fragility rather than its own panic.
    pub(crate) failed_by_fragility: bool,
}

impl Sched {
    pub(crate) fn new(unseeded: bool) -> Sched {
        Sched {
            entries: BTreeMap::new(),
            run: None,
            inherited: Vec::new(),
            queued: false,
            unseeded,
            ran_at: 0,
            subscribers: Vec::new(),
            created: Vec::new(),
            failed_by_fragility: false,
        }
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

    /// Whether the node still owes its children a release for `epoch` through a run.
    pub(crate) fn owes(&mut self, epoch: EpochId) -> bool {
        self.owed_mut(epoch).is_some()
    }

    /// Whether any run work is pending or in flight.
    pub(crate) fn busy(&self) -> bool {
        self.run.is_some() || self.queued || !self.inherited.is_empty()
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
        let mut guard = self.sched.lock();
        let mut out: Vec<NodeRef> = Vec::with_capacity(guard.subscribers.len());
        guard.subscribers.retain(|(_, weak): &(NodeId, WeakNodeRef)| match weak.upgrade() {
            Some(node) => {
                out.push(node);
                true
            }
            None => false,
        });
        drop(guard);
        out
    }

    /// Removes a freed or disposed child from the subscriber list.
    pub(crate) fn unsubscribe(&self, child: NodeId) {
        let mut guard = self.sched.lock();
        guard.subscribers.retain(|(id, _): &(NodeId, WeakNodeRef)| *id != child);
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
    pub(crate) equals: Option<Equals<K>>,
}

/// Operations the scheduler needs on a node of any value type.
pub(crate) trait AnyNode: Send + Sync + 'static {
    fn header(&self) -> &Header;
    /// Spawns a run task on the node's runtime.
    fn spawn_run(self: Arc<Self>);
    /// Drops the value where the ghost policy says so, and the body.
    fn release_payload(&self);
}

impl<K: NodeValue> AnyNode for Node<K> {
    fn header(&self) -> &Header {
        &self.header
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

    fn release_payload(&self) {
        if self.header.ghost == GhostPolicy::CauseFailure
            && let Some(value) = self.value.swap(None)
        {
            crate::drops::drop_value(value, self.header.runtime.as_ref());
        }
        let body: Option<Body<K>> = self.body.lock().take();
        if let Some(body) = body {
            crate::drops::drop_later(body, self.header.runtime.as_ref(), false);
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
        let header: &mut Header = &mut self.header;
        let ghost: bool = header.phase.load(Ordering::SeqCst) & phase::GHOST != 0;
        if header.kind == Kind::Derived
            && !ghost
            && !header.first_run_done.load(Ordering::SeqCst)
            && !crate::drops::in_value_drop()
        {
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
            slot.shared.cancel();
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
            crate::drops::drop_later(body, runtime.as_ref(), false);
        }
        if let Some(list) = parents {
            crate::drops::release_parents(list);
        }
        crate::settle_wait::bump();
    }
}
