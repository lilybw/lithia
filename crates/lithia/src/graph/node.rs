// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! The node record behind every handle, and its scheduler state (spec §5).

use std::any::TypeId;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};

use crate::sync::{ArcCell, Mutex};
use tokio::runtime::Handle;
use tokio::sync::watch;

use crate::NodeValue;
use crate::context::ClosureGuard;
use crate::graph::epoch::Tokens;
use crate::graph::ids::Stamp;
use crate::lifetime::teardown::{Detached, detach};
use crate::options::Equals;
use crate::run::Yielder;

pub(crate) use super::header::*;
pub(crate) use super::sched::*;

/// Strong, type-erased reference to a node.
pub(crate) type NodeRef = Arc<dyn AnyNode>;
/// Weak, type-erased reference to a node.
pub(crate) type WeakNodeRef = Weak<dyn AnyNode>;
/// Strong reference to a node of a known value type.
pub(crate) type TypedNodeRef<K> = Arc<Node<K>>;
/// Weak reference to a node of a known value type.
pub(crate) type WeakTypedNodeRef<K> = Weak<Node<K>>;
/// A node's parents, in parameter order.
pub(crate) type Parents = Vec<NodeRef>;
/// A node's subscribers, as one release or framing walk reaches them; inline while few.
pub(crate) type Children = smallvec::SmallVec<[NodeRef; 2]>;
/// Counts a node's published changes; its change streams watch the count.
pub(crate) type ChangeSeq = u64;
/// Wakes a node's change streams.
pub(crate) type ChangeSender = watch::Sender<ChangeSeq>;

/// Boxed future of one run.
pub(crate) type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
/// A synchronous body: produces one item per run.
pub(crate) type SyncFn<K> = Arc<dyn Fn() -> K + Send + Sync>;
/// An async body: makes one future per run.
pub(crate) type AsyncFn<K> = Arc<dyn Fn() -> BoxFut<K> + Send + Sync>;
/// A generator body: makes one future per run, which yields items through its yielder.
pub(crate) type GenFn<K> = Arc<dyn Fn(Yielder<K>) -> BoxFut<()> + Send + Sync>;
/// An async source block that yields, made once.
pub(crate) type OnceGenFn<K> = Box<dyn FnOnce(Yielder<K>) -> BoxFut<()> + Send>;
/// A node's body, empty for source nodes and once taken on disposal.
pub(crate) type BodySlot<K> = Option<Body<K>>;
/// A node's comparator, replaceable by a reclaiming creation.
pub(crate) type EqualsCell<K> = Mutex<Equals<K>>;

/// The code a node runs.
pub(crate) enum Body<K: NodeValue> {
    /// A synchronous body producing one item.
    Sync(SyncFn<K>),
    /// An async body producing one item.
    Async(AsyncFn<K>),
    /// A generator body producing items through its yielder.
    Gen(GenFn<K>),
    /// An async source block, run once.
    Once(BoxFut<K>),
    /// An async source block that yields, run once.
    OnceGen(OnceGenFn<K>),
}

/// Internal record of one node; type-erased behind every handle.
pub(crate) struct Node<K: NodeValue> {
    pub(crate) header: Header,
    /// Emptied only by disposal under `disposed = FAIL`.
    pub(crate) value: ArcCell<K>,
    /// Empty for source nodes; taken on disposal.
    pub(crate) body: Mutex<BodySlot<K>>,
    /// Empty without `equals`; a reclaiming creation replaces it (spec §10.1, Keyed nodes).
    pub(crate) equals: Option<EqualsCell<K>>,
}

impl<K: NodeValue> Node<K> {
    /// The current comparator, if the node has one.
    pub(crate) fn comparator(&self) -> Option<Equals<K>> {
        self.equals.as_ref().map(|cell: &EqualsCell<K>| cell.lock().clone())
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
    /// Polls the node's next run once on this thread; the run, boxed, if it did not finish
    /// (spec §11.4, step 4).
    fn run_here(self: Arc<Self>, cx: &mut std::task::Context<'_>) -> Option<crate::exec::fast_path::RunTask>;
    /// Drops the value where the `disposed` option says so, and the body.
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
        let future: crate::run::RunFuture<K> = crate::run::RunFuture::new(&self);
        drop(self);
        crate::exec::spawn::spawn(runtime.as_ref(), future, crate::exec::spawn::Orphan::Drop);
    }

    fn run_here(self: Arc<Self>, cx: &mut std::task::Context<'_>) -> Option<crate::exec::fast_path::RunTask> {
        let mut future: crate::run::RunFuture<K> = crate::run::RunFuture::new(&self);
        drop(self);
        // `RunFuture` is `Unpin`, so it may move into a box after this poll.
        match Pin::new(&mut future).poll(cx) {
            std::task::Poll::Ready(()) => None,
            std::task::Poll::Pending => Some(Box::pin(future)),
        }
    }

    fn release_payload(&self) {
        if self.header.shape.ghost_fails()
            && let Some(value) = self.value.swap(None)
        {
            crate::lifetime::drops::drop_value(value, self.header.runtime.as_ref());
        }
        let body: BodySlot<K> = self.body.lock().take();
        if let Some(body) = body {
            crate::lifetime::drops::drop_later(body, self.header.runtime.as_ref());
        }
    }
}

/// Whose value a commit lands: a source write, which is a run of its own, or an item of a run.
pub(crate) enum Commit<'a> {
    Write,
    Item(&'a crate::run::RunControl),
}

/// Values a commit displaced, dropped once the write lock is released.
pub(crate) struct Displaced<K> {
    pub(crate) discarded: Option<K>,
    pub(crate) previous: Option<Arc<K>>,
}

impl<K> Default for Displaced<K> {
    fn default() -> Displaced<K> {
        Displaced { discarded: None, previous: None }
    }
}

impl<K: NodeValue> Displaced<K> {
    /// Drops what the commit displaced off the protocol (spec §10.3).
    pub(crate) fn release(self, runtime: Option<&Handle>) {
        if let Some(value) = self.discarded {
            crate::lifetime::drops::drop_owned(value, runtime);
        }
        if let Some(old) = self.previous {
            crate::lifetime::drops::drop_value(old, runtime);
        }
    }
}

impl<K: NodeValue> Node<K> {
    /// Lands `next` over `current` under the node's write lock, unless the comparator finds them
    /// equal; returns the change's stamp (spec §6.1, §6.4).
    pub(crate) fn commit(
        &self,
        current: &Arc<K>,
        next: K,
        commit: Commit<'_>,
        displaced: &mut Displaced<K>,
    ) -> Option<Stamp> {
        let header: &Header = &self.header;
        let equal: bool = match self.comparator() {
            Some(equals) => {
                let _guard: ClosureGuard = ClosureGuard::enter();
                equals(current, &next)
            }
            None => false,
        };
        if let Commit::Item(_) = commit {
            header.clear_bits(phase::PENDING);
        }
        if equal {
            displaced.discarded = Some(next);
            return None;
        }
        if let Commit::Write = commit {
            // A write is a run whose only item is the new value.
            header.runs.advance(Ordering::SeqCst);
            header.clear_pending_failed();
        }
        displaced.previous = self.value.swap(Some(Arc::new(next)));
        let stamp: Stamp = crate::graph::ids::stamp();
        debug_assert!(stamp > header.version.load(Ordering::Relaxed), "lithia: a node's version went back");
        // Readers load it with at least acquire ordering.
        header.version.store(stamp, Ordering::Release);
        if let Commit::Item(run) = commit {
            run.note_stamp(stamp);
        }
        header.notify_changes();
        Some(stamp)
    }

    pub(crate) fn is_unit() -> bool {
        TypeId::of::<K>() == TypeId::of::<()>()
    }
}

impl<K: NodeValue> Drop for Node<K> {
    fn drop(&mut self) {
        let _op: crate::lifetime::op::Operation = crate::lifetime::op::enter();
        let header: &mut Header = &mut self.header;
        let ghost: bool = header.phase.load(Ordering::SeqCst) & phase::GHOST != 0;
        if header.diag.warn_first_run && !ghost && !header.diag.under_way.load(Ordering::Acquire) {
            crate::diag::freed_before_first_run(header.site);
        }
        let detached: Detached = detach(header);
        let sched: &mut Sched = header.sched.get_mut();
        let mut tokens: Tokens = Tokens::new();
        for (_, entry) in std::mem::take(&mut sched.entries) {
            tokens.push(entry.epoch);
        }
        if let Some(slot) = sched.run.take() {
            // Woken, a parked run finds its node gone and drops its future, captures included.
            slot.shared.cancel();
            slot.shared.wake();
            for served in slot.served {
                tokens.add(served.epoch, served.tokens);
            }
        }
        for served in std::mem::take(&mut sched.handover) {
            tokens.add(served.epoch, served.tokens);
        }
        sched.subscribers.clear();
        sched.trace.clear();
        let generation: Vec<NodeRef> = sched.release_all();
        tokens.close();
        let runtime: Option<Handle> = header.runtime.clone();
        if let Some(value) = self.value.swap(None) {
            crate::lifetime::drops::drop_value(value, runtime.as_ref());
        }
        if let Some(body) = self.body.get_mut().take() {
            crate::lifetime::drops::drop_later(body, runtime.as_ref());
        }
        // Comparator, key and kept inputs carry user drop glue, which teardown never runs inline.
        if let Some(equals) = self.equals.take() {
            crate::lifetime::drops::drop_later(equals, runtime.as_ref());
        }
        if let Some(key) = self.header.keyed.key.take() {
            crate::lifetime::drops::drop_later(key, runtime.as_ref());
        }
        if let Some(inputs) = self.header.keyed.inputs.get_mut().take() {
            inputs.release(runtime.as_ref());
        }
        // A freed writer no longer supports its targets; its generation loses its creation edges.
        for target in self.header.targets.iter().filter_map(WeakNodeRef::upgrade) {
            crate::lifetime::support::raise(target);
        }
        detached.release(generation);
        crate::lifetime::support::freed(&self.header);
        crate::testing::progress::bump();
    }
}
