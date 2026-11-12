//! Runs of node bodies, generator yields, and lithia tasks (spec §3.6, §6.2, §7.3, §9.1).

use std::any::Any;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll, Waker};

use crate::sync::{AtomicBool, AtomicU64, Mutex, MutexGuard};

use crate::NodeValue;
use crate::context::{self, Batch, ClosureGuard, Frame, FrameGuard, Target};
use crate::epoch::{Epoch, Scope};
use crate::ids::{self, NodeId};
use crate::node::{Body, BoxFut, Header, Node, NodeRef, WeakNodeRef};
use crate::protocol::{self, End, Outcome};

/// State of one run shared between its task, its yielder and the scheduler.
pub(crate) struct RunShared {
    pub(crate) node: WeakNodeRef,
    pub(crate) node_id: NodeId,
    scope: Mutex<Arc<Scope>>,
    id: AtomicU64,
    /// Run id an async source block was registered under at creation, or 0.
    expected: u64,
    cancelled: AtomicBool,
    waker: Mutex<Option<Waker>>,
    first_done: AtomicBool,
    yielded: AtomicBool,
    /// Newest change stamp of the node's own items during the current poll.
    stamp: AtomicU64,
}

impl RunShared {
    pub(crate) fn new(node: WeakNodeRef, node_id: NodeId, scope: Arc<Scope>, expected: u64) -> RunShared {
        RunShared {
            node,
            node_id,
            scope: Mutex::new(scope),
            id: AtomicU64::new(0),
            expected,
            cancelled: AtomicBool::new(false),
            waker: Mutex::new(None),
            first_done: AtomicBool::new(false),
            yielded: AtomicBool::new(false),
            stamp: AtomicU64::new(0),
        }
    }

    pub(crate) fn id(&self) -> u64 {
        self.id.load(Ordering::SeqCst)
    }

    pub(crate) fn set_id(&self, id: u64) {
        self.id.store(id, Ordering::SeqCst);
    }

    pub(crate) fn expected(&self) -> u64 {
        self.expected
    }

    pub(crate) fn scope(&self) -> Arc<Scope> {
        self.scope.lock().clone()
    }

    pub(crate) fn set_scope(&self, scope: Arc<Scope>) {
        *self.scope.lock() = scope;
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

    fn set_waker(&self, waker: &Waker) {
        let mut slot: MutexGuard<'_, Option<Waker>> = self.waker.lock();
        match slot.as_ref() {
            Some(existing) if existing.will_wake(waker) => {}
            _ => *slot = Some(waker.clone()),
        }
    }

    pub(crate) fn first_done(&self) -> bool {
        self.first_done.load(Ordering::SeqCst)
    }

    pub(crate) fn set_first_done(&self) {
        self.first_done.store(true, Ordering::SeqCst);
    }

    /// Records a change stamp produced by the node's own item.
    pub(crate) fn note_stamp(&self, stamp: u64) {
        self.stamp.fetch_max(stamp, Ordering::SeqCst);
    }

    fn begin_poll(&self) {
        self.yielded.store(false, Ordering::SeqCst);
        self.stamp.store(0, Ordering::SeqCst);
    }
}

/// Extracts the message of a caught panic.
pub(crate) fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "panic with a non-string payload".to_string()
    }
}

enum RunState<K: NodeValue> {
    /// Not started yet.
    Init,
    /// A synchronous body, called within one poll.
    Sync(Arc<dyn Fn() -> K + Send + Sync>),
    /// An async body, a generator or an async source block.
    Future(BoxFut<Option<K>>),
    /// Ended, aborted or never started.
    Done,
}

/// The task of one run; holds its node weakly (spec §10.1).
pub(crate) struct RunFuture<K: NodeValue> {
    node: Weak<Node<K>>,
    shared: Arc<RunShared>,
    state: RunState<K>,
    polls: u32,
}

impl<K: NodeValue> RunFuture<K> {
    pub(crate) fn new(node: Weak<Node<K>>) -> RunFuture<K> {
        let (node_id, scope, expected): (NodeId, Arc<Scope>, u64) = match node.upgrade() {
            Some(strong) => {
                let header: &Header = &strong.header;
                let expected: u64 = if header.kind == crate::node::Kind::AsyncSource { 1 } else { 0 };
                (header.id, header.scope.clone(), expected)
            }
            None => (0, Scope::new(), 0),
        };
        let weak: WeakNodeRef = node.clone();
        RunFuture {
            node,
            shared: Arc::new(RunShared::new(weak, node_id, scope, expected)),
            state: RunState::Init,
            polls: 0,
        }
    }

    /// Builds the run's state from the node's body.
    fn prepare(&self, node: &Arc<Node<K>>) -> RunState<K> {
        let mut body: MutexGuard<'_, Option<Body<K>>> = node.body.lock();
        let yielder = || Yielder { shared: self.shared.clone(), node: Arc::downgrade(node) };
        match body.as_ref() {
            Some(Body::Sync(f)) => return RunState::Sync(f.clone()),
            Some(Body::Async(f)) => {
                let f: Arc<dyn Fn() -> BoxFut<K> + Send + Sync> = f.clone();
                drop(body);
                return RunState::Future(Box::pin(async move { Some(f().await) }));
            }
            Some(Body::Gen(f)) => {
                let f: Arc<dyn Fn(Yielder<K>) -> BoxFut<()> + Send + Sync> = f.clone();
                drop(body);
                let generator: BoxFut<()> = f(yielder());
                return RunState::Future(Box::pin(async move {
                    generator.await;
                    None
                }));
            }
            _ => {}
        }
        match body.take() {
            Some(Body::Once(future)) => RunState::Future(Box::pin(async move { Some(future.await) })),
            Some(Body::OnceGen(start)) => {
                drop(body);
                let generator: BoxFut<()> = start(yielder());
                RunState::Future(Box::pin(async move {
                    generator.await;
                    None
                }))
            }
            _ => RunState::Done,
        }
    }

    fn stop(&mut self) {
        let state: RunState<K> = std::mem::replace(&mut self.state, RunState::Done);
        drop(state);
    }
}

impl<K: NodeValue> Future for RunFuture<K> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this: &mut RunFuture<K> = self.get_mut();
        // Every strong reference to the node is gone before any token closes, so a settled
        // scope never finds the node alive only because its run task is finishing (spec §10.1).
        let (polled, closes): (Poll<()>, Vec<Arc<Epoch>>) = this.poll_run(cx);
        protocol::close_all(closes);
        polled
    }
}

impl<K: NodeValue> RunFuture<K> {
    fn poll_run(&mut self, cx: &mut Context<'_>) -> (Poll<()>, Vec<Arc<Epoch>>) {
        if matches!(self.state, RunState::Done) {
            return (Poll::Ready(()), Vec::new());
        }
        self.shared.set_waker(cx.waker());
        let Some(node) = self.node.upgrade() else {
            self.stop();
            return (Poll::Ready(()), Vec::new());
        };
        let erased: NodeRef = node.clone();
        let mut closes: Vec<Arc<Epoch>> = Vec::new();
        if matches!(self.state, RunState::Init) {
            let (started, pending): (bool, Vec<Arc<Epoch>>) = protocol::start_run(&erased, &self.shared);
            closes.extend(pending);
            if !started {
                self.stop();
                return (Poll::Ready(()), closes);
            }
            let frame: Frame = Frame::new(Target::Run(self.shared.clone()));
            let prepared: Result<RunState<K>, Box<dyn Any + Send>> = {
                let _entered: FrameGuard<'_> = context::enter(&frame);
                catch_unwind(AssertUnwindSafe(|| self.prepare(&node)))
            };
            match prepared {
                Ok(state) => self.state = state,
                Err(panic) => {
                    let batch: Batch = frame.take_batch();
                    let outcome: Outcome = Outcome::Panicked(panic_message(&*panic));
                    self.stop();
                    closes.extend(protocol::after_poll(&erased, &self.shared, batch, &outcome, 0));
                    return (Poll::Ready(()), closes);
                }
            }
            if matches!(self.state, RunState::Done) {
                closes.extend(protocol::after_poll(&erased, &self.shared, frame.take_batch(), &Outcome::Ended, 0));
                return (Poll::Ready(()), closes);
            }
        } else if self.shared.is_cancelled() {
            self.stop();
            closes.extend(protocol::finalize(&erased, &self.shared, End::Finished));
            return (Poll::Ready(()), closes);
        }

        self.shared.begin_poll();
        let frame: Frame = Frame::new(Target::Run(self.shared.clone()));
        let outcome: Outcome = {
            let _entered: FrameGuard<'_> = context::enter(&frame);
            let state: &mut RunState<K> = &mut self.state;
            let polled: Result<Poll<Option<K>>, Box<dyn Any + Send>> = catch_unwind(AssertUnwindSafe(|| match state {
                RunState::Sync(f) => Poll::Ready(Some(f())),
                RunState::Future(future) => future.as_mut().poll(cx),
                RunState::Init | RunState::Done => Poll::Ready(None),
            }));
            match polled {
                Ok(Poll::Ready(Some(value))) => {
                    let shared: &RunShared = &self.shared;
                    match catch_unwind(AssertUnwindSafe(|| node.publish(shared, value))) {
                        Ok(()) => Outcome::Ended,
                        Err(panic) => Outcome::Panicked(panic_message(&*panic)),
                    }
                }
                Ok(Poll::Ready(None)) => Outcome::Ended,
                Ok(Poll::Pending) => {
                    if self.shared.yielded.swap(false, Ordering::SeqCst) {
                        Outcome::Yielded
                    } else {
                        Outcome::Suspended
                    }
                }
                Err(panic) => Outcome::Panicked(panic_message(&*panic)),
            }
        };
        let batch: Batch = frame.take_batch();
        drop(frame);
        self.polls = self.polls.saturating_add(1);
        let ended: bool = matches!(outcome, Outcome::Ended | Outcome::Panicked(_));
        if ended || self.polls >= 2 || matches!(outcome, Outcome::Yielded) {
            node.header.under_way.store(true, Ordering::SeqCst);
        }
        if ended {
            self.stop();
        }
        let stamp: u64 = self.shared.stamp.swap(0, Ordering::SeqCst);
        closes.extend(protocol::after_poll(&erased, &self.shared, batch, &outcome, stamp));
        if ended {
            return (Poll::Ready(()), closes);
        }
        if self.shared.is_cancelled() {
            self.stop();
            closes.extend(protocol::finalize(&erased, &self.shared, End::Finished));
            return (Poll::Ready(()), closes);
        }
        (Poll::Pending, closes)
    }
}

impl<K: NodeValue> Drop for RunFuture<K> {
    fn drop(&mut self) {
        if matches!(self.state, RunState::Done) {
            return;
        }
        // Dropped before finishing: the runtime shut down (spec §11.3).
        let state: RunState<K> = std::mem::replace(&mut self.state, RunState::Done);
        let closes: Vec<Arc<Epoch>> = match self.node.upgrade() {
            Some(node) => {
                let erased: NodeRef = node;
                protocol::abandon(&erased, &self.shared)
            }
            None => Vec::new(),
        };
        crate::drops::drop_later(state, None);
        protocol::close_all(closes);
    }
}

/// Publishes a generator body's items; `yield x` lowers to `yielder.item(x).await` (spec §16).
#[doc(hidden)]
pub struct Yielder<K: NodeValue> {
    shared: Arc<RunShared>,
    node: Weak<Node<K>>,
}

impl<K: NodeValue> Yielder<K> {
    /// Publishes `value` as the node's latest item, then ends the run's current poll.
    pub fn item(&self, value: K) -> YieldItem<'_, K> {
        YieldItem { yielder: self, value: Some(value) }
    }
}

/// Future of one `yield`: publishes on its first poll and completes on its second.
#[doc(hidden)]
pub struct YieldItem<'a, K: NodeValue> {
    yielder: &'a Yielder<K>,
    value: Option<K>,
}

impl<K: NodeValue> Unpin for YieldItem<'_, K> {}

impl<K: NodeValue> Future for YieldItem<'_, K> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this: &mut YieldItem<'_, K> = self.get_mut();
        match this.value.take() {
            Some(value) => {
                match this.yielder.node.upgrade() {
                    Some(node) => node.publish(&this.yielder.shared, value),
                    None => crate::drops::drop_later(value, None),
                }
                this.yielder.shared.yielded.store(true, Ordering::SeqCst);
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            None => Poll::Ready(()),
        }
    }
}

impl<K: NodeValue> Node<K> {
    /// Publishes an item of `run`, unless the run was superseded (spec §6.4, §7.3 item 4).
    pub(crate) fn publish(&self, run: &RunShared, value: K) {
        let header: &Header = &self.header;
        let mut discarded: Option<K> = None;
        let mut replaced: Option<Arc<K>> = None;
        {
            let _write: MutexGuard<'_, ()> = header.write.lock();
            let current: Option<Arc<K>> = self.value.load_full();
            if run.is_cancelled() || header.runs.load(Ordering::SeqCst) != run.id() || header.is_ghost() {
                discarded = Some(value);
            } else if let Some(current) = current {
                let equal: bool = match &self.equals {
                    Some(equals) => {
                        let _guard: ClosureGuard = ClosureGuard::enter();
                        equals(&current, &value)
                    }
                    None => false,
                };
                header.clear_bits(crate::node::phase::PENDING);
                if equal {
                    discarded = Some(value);
                    if header.flags.contains(crate::Flags::ALWAYS_PROPAGATE) {
                        let stamp: u64 = ids::stamp();
                        header.version.store(stamp, Ordering::SeqCst);
                        run.note_stamp(stamp);
                    }
                } else {
                    replaced = self.value.swap(Some(Arc::new(value)));
                    let stamp: u64 = ids::stamp();
                    header.version.store(stamp, Ordering::SeqCst);
                    run.note_stamp(stamp);
                    header.notify_changes();
                }
            } else {
                discarded = Some(value);
            }
        }
        if let Some(value) = discarded {
            crate::drops::drop_later(value, header.runtime.as_ref());
        }
        if let Some(old) = replaced {
            crate::drops::drop_value(old, header.runtime.as_ref());
        }
    }
}

/// A task started by `lithia::spawn`, `#[lithia::main]` or `#[lithia::test]`: each poll is a batch.
pub struct TaskFuture<F> {
    scope: Arc<Scope>,
    future: Pin<Box<F>>,
}

impl<F: Future> TaskFuture<F> {
    pub(crate) fn new(scope: Arc<Scope>, future: F) -> TaskFuture<F> {
        TaskFuture { scope, future: Box::pin(future) }
    }
}

impl<F: Future> Future for TaskFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this: &mut TaskFuture<F> = self.get_mut();
        let frame: Frame = Frame::new(Target::Scope(this.scope.clone()));
        struct Deliver<'a> {
            frame: &'a Frame,
            scope: &'a Arc<Scope>,
        }
        impl Drop for Deliver<'_> {
            fn drop(&mut self) {
                let batch: Batch = self.frame.take_batch();
                protocol::deliver_outside(batch, self.scope);
            }
        }
        let deliver: Deliver<'_> = Deliver { frame: &frame, scope: &this.scope };
        let polled: Poll<F::Output> = {
            let _entered: FrameGuard<'_> = context::enter(&frame);
            this.future.as_mut().poll(cx)
        };
        drop(deliver);
        polled
    }
}

/// Starts a lithia task in a new scope; its polls form batches (spec §3.5).
pub(crate) fn scoped<F: Future>(future: F) -> TaskFuture<F> {
    TaskFuture::new(Scope::new(), future)
}

#[doc(hidden)]
impl<F> std::fmt::Debug for TaskFuture<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskFuture").field("scope", &self.scope.id).finish()
    }
}
