//! Where lithia is running on this thread: frames, batches and scopes (spec §6.2, §11.3).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::{Arc, OnceLock, Weak};

use crate::epoch::{Epoch, Scope};
use crate::ids::NodeId;
use crate::node::{NodeRef, WeakNodeRef};
use crate::run::RunShared;

/// A function deferred until an epoch settles.
pub(crate) struct Deferred {
    pub(crate) f: Box<dyn FnOnce() + Send>,
    pub(crate) scope: Arc<Scope>,
    pub(crate) runtime: Option<tokio::runtime::Handle>,
}

/// Writes, node creations, reflows and deferrals made between two batch boundaries.
#[derive(Default)]
pub(crate) struct Batch {
    /// Written source nodes and the stamp each write took.
    pub(crate) writes: Vec<(WeakNodeRef, NodeId, u64)>,
    /// Reflow targets and the stamp each reflow took.
    pub(crate) reflows: Vec<(WeakNodeRef, NodeId, u64)>,
    /// New derived and async source nodes.
    pub(crate) creations: Vec<WeakNodeRef>,
    /// Surviving children of a disposed node, which rerun.
    pub(crate) reruns: Vec<WeakNodeRef>,
    pub(crate) defers: Vec<Deferred>,
}

impl Batch {
    pub(crate) fn is_empty(&self) -> bool {
        self.writes.is_empty()
            && self.reflows.is_empty()
            && self.creations.is_empty()
            && self.reruns.is_empty()
            && self.defers.is_empty()
    }
}

/// Where blocking work delivers its batch.
#[derive(Clone)]
pub(crate) enum BlockingTarget {
    /// A body's run: child epochs while it is in flight, an outside batch after.
    Run(Arc<RunShared>),
    /// An outside batch in this scope.
    Scope(Arc<Scope>),
}

/// What is being polled on this thread.
pub(crate) enum FrameKind {
    /// A task started by `lithia::spawn`, `#[lithia::main]` or `#[lithia::test]`.
    Task(Arc<Scope>),
    /// One poll of a node's run.
    Run(Arc<RunShared>),
    /// A `lithia::spawn_blocking` closure.
    Blocking(BlockingTarget),
}

/// One batch being collected.
pub(crate) struct Frame {
    pub(crate) kind: FrameKind,
    pub(crate) batch: RefCell<Batch>,
}

impl Frame {
    pub(crate) fn new(kind: FrameKind) -> Frame {
        Frame { kind, batch: RefCell::new(Batch::default()) }
    }

    pub(crate) fn take_batch(&self) -> Batch {
        std::mem::take(&mut *self.batch.borrow_mut())
    }

    pub(crate) fn scope(&self) -> Arc<Scope> {
        match &self.kind {
            FrameKind::Task(scope) => scope.clone(),
            FrameKind::Run(run) => run.scope(),
            FrameKind::Blocking(BlockingTarget::Run(run)) => run.scope(),
            FrameKind::Blocking(BlockingTarget::Scope(scope)) => scope.clone(),
        }
    }
}

crate::sync::thread_local! {
    static CURRENT: Cell<*const Frame> = Cell::new(std::ptr::null());
    static CLOSURE_DEPTH: Cell<u32> = Cell::new(0);
    static CLOSURE_CREATIONS: RefCell<Vec<WeakNodeRef>> = RefCell::new(Vec::new());
    static THREAD_SCOPE: RefCell<Option<Arc<Scope>>> = RefCell::new(None);
}

/// Restores the previous frame when dropped.
pub(crate) struct FrameGuard<'a> {
    previous: *const Frame,
    _frame: PhantomData<&'a Frame>,
}

/// Makes `frame` current on this thread until the guard drops.
pub(crate) fn enter(frame: &Frame) -> FrameGuard<'_> {
    let previous: *const Frame = CURRENT.with(|current: &Cell<*const Frame>| current.replace(frame));
    FrameGuard { previous, _frame: PhantomData }
}

impl Drop for FrameGuard<'_> {
    fn drop(&mut self) {
        CURRENT.with(|current: &Cell<*const Frame>| current.set(self.previous));
    }
}

/// Calls `f` with the current frame, if any.
pub(crate) fn with_frame<R>(f: impl FnOnce(Option<&Frame>) -> R) -> R {
    let pointer: *const Frame = CURRENT.try_with(Cell::get).unwrap_or(std::ptr::null());
    // SAFETY: a non-null pointer was set by `enter`, whose guard keeps the frame alive
    // and restores the previous pointer before the frame goes out of scope.
    let frame: Option<&Frame> = unsafe { pointer.as_ref() };
    f(frame)
}

/// The run that creates here: the one being polled, or the one whose blocking work this is.
pub(crate) fn creating_run() -> Option<Arc<RunShared>> {
    with_frame(|frame: Option<&Frame>| match frame.map(|f: &Frame| &f.kind) {
        Some(FrameKind::Run(run)) | Some(FrameKind::Blocking(BlockingTarget::Run(run))) => Some(run.clone()),
        _ => None,
    })
}

/// The scope of the current task or thread (spec §11.3).
pub(crate) fn current_scope() -> Arc<Scope> {
    with_frame(|frame: Option<&Frame>| match frame {
        Some(frame) => frame.scope(),
        None => detached_scope(),
    })
}

/// The scope of a task or thread lithia did not start.
pub(crate) fn detached_scope() -> Arc<Scope> {
    if let Some(id) = tokio::task::try_id() {
        return task_scope(id);
    }
    THREAD_SCOPE
        .try_with(|slot: &RefCell<Option<Arc<Scope>>>| slot.borrow_mut().get_or_insert_with(Scope::new).clone())
        .unwrap_or_else(|_| Scope::new())
}

fn task_scope(id: tokio::task::Id) -> Arc<Scope> {
    static SCOPES: OnceLock<parking_lot::Mutex<HashMap<tokio::task::Id, Weak<Scope>>>> = OnceLock::new();
    let mut map: parking_lot::MutexGuard<'_, HashMap<tokio::task::Id, Weak<Scope>>> =
        SCOPES.get_or_init(|| parking_lot::Mutex::new(HashMap::new())).lock();
    if let Some(scope) = map.get(&id).and_then(Weak::upgrade) {
        return scope;
    }
    if map.len() >= 64 {
        map.retain(|_, weak: &mut Weak<Scope>| weak.strong_count() > 0);
    }
    // Open epochs hold their scope, so a flush finds it while work remains; once the
    // scope is gone there is nothing left to wait for, and a fresh one is equivalent.
    let scope: Arc<Scope> = Scope::new();
    map.insert(id, Arc::downgrade(&scope));
    scope
}

/// Marks the current thread as running a user closure that may not write (spec §6.1).
pub(crate) struct ClosureGuard(());

impl ClosureGuard {
    pub(crate) fn enter() -> ClosureGuard {
        CLOSURE_DEPTH.with(|depth: &Cell<u32>| depth.set(depth.get() + 1));
        ClosureGuard(())
    }
}

impl Drop for ClosureGuard {
    fn drop(&mut self) {
        CLOSURE_DEPTH.with(|depth: &Cell<u32>| depth.set(depth.get() - 1));
    }
}

/// Panics if called inside an `update`, `modify` or `equals` closure.
pub(crate) fn assert_not_in_closure(operation: &str) {
    let depth: u32 = CLOSURE_DEPTH.try_with(Cell::get).unwrap_or(0);
    if depth > 0 {
        panic!(
            "lithia: {operation} inside an update, modify or equals closure is not allowed; \
             it could deadlock against another thread's write lock"
        );
    }
}

/// Delivers node creations made inside a closure once the write lock is released.
pub(crate) struct ClosureCreations(());

impl ClosureCreations {
    pub(crate) fn new() -> ClosureCreations {
        ClosureCreations(())
    }
}

impl Drop for ClosureCreations {
    fn drop(&mut self) {
        if CLOSURE_DEPTH.try_with(Cell::get).unwrap_or(0) != 0 {
            return;
        }
        let creations: Vec<WeakNodeRef> = CLOSURE_CREATIONS
            .try_with(|list: &RefCell<Vec<WeakNodeRef>>| std::mem::take(&mut *list.borrow_mut()))
            .unwrap_or_default();
        if !creations.is_empty() {
            let batch: Batch = Batch { creations, ..Batch::default() };
            crate::protocol::deliver_outside(batch, &detached_scope());
        }
    }
}

/// Records a change to a source node in the current batch, or delivers it at once.
pub(crate) fn record_write(node: &NodeRef, stamp: u64) {
    record(|batch: &mut Batch| batch.writes.push((Arc::downgrade(node), node.header().id, stamp)));
}

/// Records a reflow in the current batch, or delivers it at once.
pub(crate) fn record_reflow(node: &NodeRef, stamp: u64) {
    record(|batch: &mut Batch| batch.reflows.push((Arc::downgrade(node), node.header().id, stamp)));
}

/// Records a new derived or async source node, which gets its first run at delivery.
pub(crate) fn record_creation(node: &NodeRef) {
    let weak: WeakNodeRef = Arc::downgrade(node);
    let has_frame: bool = with_frame(|frame: Option<&Frame>| frame.is_some());
    let depth: u32 = CLOSURE_DEPTH.try_with(Cell::get).unwrap_or(0);
    if !has_frame && depth > 0 {
        CLOSURE_CREATIONS.with(|list: &RefCell<Vec<WeakNodeRef>>| list.borrow_mut().push(weak));
        return;
    }
    record(move |batch: &mut Batch| batch.creations.push(weak));
}

/// Records surviving children of a disposed node, which rerun.
pub(crate) fn record_reruns(nodes: Vec<WeakNodeRef>) {
    if nodes.is_empty() {
        return;
    }
    record(move |batch: &mut Batch| batch.reruns.extend(nodes));
}

/// Records a deferred function in the current batch, or attaches it to the latest epoch.
pub(crate) fn record_defer(deferred: Deferred) {
    let pending: Option<Deferred> = with_frame(move |frame: Option<&Frame>| match frame {
        Some(frame) => {
            frame.batch.borrow_mut().defers.push(deferred);
            None
        }
        None => Some(deferred),
    });
    if let Some(deferred) = pending {
        let epoch: Option<Arc<Epoch>> = deferred.scope.last();
        match epoch {
            Some(epoch) => crate::protocol::attach_defers(vec![epoch], vec![deferred]),
            None => crate::protocol::fire_deferred(deferred),
        }
    }
}

fn record(push: impl FnOnce(&mut Batch)) {
    let detached: Option<Batch> = with_frame(move |frame: Option<&Frame>| match frame {
        Some(frame) => {
            push(&mut frame.batch.borrow_mut());
            None
        }
        None => {
            let mut batch: Batch = Batch::default();
            push(&mut batch);
            Some(batch)
        }
    });
    if let Some(batch) = detached {
        crate::protocol::deliver_outside(batch, &detached_scope());
    }
}
