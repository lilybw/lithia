// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! The run future: starts a run, polls its body under its frame, publishes items and ends it (spec §8).

use super::*;

/// A panic's payload, as `catch_unwind` returns it.
pub(crate) type PanicPayload = Box<dyn Any + Send>;

/// One poll of a body: an item, the end of a generator, or not yet.
pub(super) type Polled<K> = Poll<Option<K>>;

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

pub(super) enum RunState<K: NodeValue> {
    /// Not started yet.
    Init,
    /// A synchronous body, called within one poll.
    Sync(SyncFn<K>),
    /// An async body or an async source block, which ends with its item.
    Item(BoxFut<K>),
    /// A generator, whose items go out through its yielder.
    Items(BoxFut<()>),
    /// Ended, aborted or never started.
    Done,
}

/// The task of one run; holds its node weakly (spec §10.1).
pub(crate) struct RunFuture<K: NodeValue> {
    pub(super) node: WeakTypedNodeRef<K>,
    /// The runtime its task runs on, for the fast path.
    pub(super) runtime: crate::exec::fast_path::RuntimeKey,
    pub(super) shared: Arc<RunControl>,
    pub(super) state: RunState<K>,
    pub(super) polls: u32,
}

impl<K: NodeValue> RunFuture<K> {
    pub(crate) fn new(strong: &TypedNodeRef<K>) -> RunFuture<K> {
        let header: &Header = &strong.header;
        let expected: RunId = if header.kind == crate::graph::node::Kind::AsyncSource {
            header.block_run.load(Ordering::SeqCst)
        } else {
            RunId::NONE
        };
        let node: WeakTypedNodeRef<K> = Arc::downgrade(strong);
        let weak: WeakNodeRef = node.clone();
        RunFuture {
            node,
            runtime: crate::exec::fast_path::runtime_key(header),
            shared: Arc::new(RunControl::new(weak, Some(header.id), header.scope.clone(), expected)),
            state: RunState::Init,
            polls: 0,
        }
    }

    /// Builds the run's state from the node's body.
    fn prepare(&self, node: &TypedNodeRef<K>) -> RunState<K> {
        let mut body: MutexGuard<'_, BodySlot<K>> = node.body.lock();
        let yielder = || Yielder { shared: self.shared.clone(), node: Arc::downgrade(node) };
        match body.as_ref() {
            Some(Body::Sync(f)) => return RunState::Sync(f.clone()),
            // The bodies' futures are boxed already; their output is mapped as they are polled.
            Some(Body::Async(f)) => {
                let f: AsyncFn<K> = f.clone();
                drop(body);
                return RunState::Item(f());
            }
            Some(Body::Gen(f)) => {
                let f: GenFn<K> = f.clone();
                drop(body);
                return RunState::Items(f(yielder()));
            }
            _ => {}
        }
        match body.take() {
            Some(Body::Once(future)) => RunState::Item(future),
            Some(Body::OnceGen(start)) => {
                drop(body);
                RunState::Items(start(yielder()))
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
        // The poll's deferred drops are queued together when it ends (spec §10.3).
        let _drops: crate::lifetime::drops::DropBatch = crate::lifetime::drops::gather();
        // A run task runs the ready runs its step hands it next (spec §11.4, step 4); a run it
        // polls that way opens nothing of its own.
        let open: Option<crate::exec::fast_path::Open> = crate::exec::fast_path::open(this.runtime);
        // Every strong reference to the node is gone before any token closes, so a settled
        // scope never finds the node alive only because its run task is finishing (spec §10.1).
        let (polled, closes): (Poll<()>, Tokens) = this.poll_run(cx);
        match &open {
            Some(open) => {
                closes.close();
                open.run(cx, &mut std::collections::VecDeque::new());
            }
            None => crate::exec::fast_path::close_later(closes),
        }
        polled
    }
}

impl<K: NodeValue> RunFuture<K> {
    fn poll_run(&mut self, cx: &mut Context<'_>) -> (Poll<()>, Tokens) {
        if matches!(self.state, RunState::Done) {
            return (Poll::Ready(()), Tokens::new());
        }
        // Only a run that can suspend is woken: a synchronous body ends within its first poll.
        if matches!(self.state, RunState::Item(_) | RunState::Items(_)) {
            self.shared.set_waker(cx.waker());
        }
        let Some(node) = self.node.upgrade() else {
            self.stop();
            return (Poll::Ready(()), Tokens::new());
        };
        let erased: NodeRef = node.clone();
        let mut closes: Tokens = Tokens::new();
        if matches!(self.state, RunState::Init) {
            let (started, pending): (bool, Tokens) = protocol::start_run(&erased, &self.shared);
            closes.append(pending);
            if !started {
                self.stop();
                return (Poll::Ready(()), closes);
            }
            let frame: Frame = Frame::new(Target::Run(self.shared.clone()));
            let prepared: Result<RunState<K>, PanicPayload> =
                context::entered(&frame, || catch_unwind(AssertUnwindSafe(|| self.prepare(&node))));
            match prepared {
                Ok(state) => self.state = state,
                Err(panic) => {
                    let batch: Batch = frame.take_batch();
                    let outcome: Outcome = Outcome::Panicked(panic_message(&*panic));
                    self.stop();
                    closes.append(protocol::after_poll(&erased, &self.shared, batch, &outcome, Stamp::NONE));
                    return (Poll::Ready(()), closes);
                }
            }
            if matches!(self.state, RunState::Done) {
                closes.append(protocol::after_poll(
                    &erased,
                    &self.shared,
                    frame.take_batch(),
                    &Outcome::Ended,
                    Stamp::NONE,
                ));
                return (Poll::Ready(()), closes);
            }
            // A cancel before this point finds no waker; the check after the poll catches it.
            if matches!(self.state, RunState::Item(_) | RunState::Items(_)) {
                self.shared.set_waker(cx.waker());
            }
        } else if self.shared.is_cancelled() {
            self.stop();
            closes.append(protocol::finalize(&erased, &self.shared, End::Finished));
            return (Poll::Ready(()), closes);
        }

        self.shared.begin_poll();
        let frame: Frame = Frame::new(Target::Run(self.shared.clone()));
        let state: &mut RunState<K> = &mut self.state;
        let shared: &RunControl = &self.shared;
        let outcome: Outcome = context::entered(&frame, || {
            let polled: Result<Polled<K>, PanicPayload> = {
                let _paused: crate::exec::fast_path::Paused = crate::exec::fast_path::pause();
                // A panic in the body ends the run and fails its node (spec §9.1).
                catch_unwind(AssertUnwindSafe(|| match state {
                    RunState::Sync(f) => Poll::Ready(Some(f())),
                    RunState::Item(future) => future.as_mut().poll(cx).map(Some),
                    RunState::Items(future) => future.as_mut().poll(cx).map(|()| None),
                    RunState::Init | RunState::Done => Poll::Ready(None),
                }))
            };
            match polled {
                Ok(Poll::Ready(Some(value))) => match catch_unwind(AssertUnwindSafe(|| node.publish(shared, value))) {
                    Ok(()) => Outcome::Ended,
                    Err(panic) => Outcome::Panicked(panic_message(&*panic)),
                },
                Ok(Poll::Ready(None)) => Outcome::Ended,
                Ok(Poll::Pending) => {
                    if shared.yielded.swap(false, Ordering::SeqCst) {
                        Outcome::Yielded
                    } else {
                        Outcome::Suspended
                    }
                }
                Err(panic) => Outcome::Panicked(panic_message(&*panic)),
            }
        });
        let batch: Batch = frame.take_batch();
        drop(frame);
        self.polls = self.polls.saturating_add(1);
        let ended: bool = matches!(outcome, Outcome::Ended | Outcome::Panicked(_));
        if ended || self.polls >= 2 || matches!(outcome, Outcome::Yielded) {
            node.header.diag.under_way.store(true, Ordering::Release);
        }
        if ended {
            self.stop();
        }
        let stamp: Stamp = self.shared.stamp.swap(Stamp::NONE, Ordering::SeqCst);
        closes.append(protocol::after_poll(&erased, &self.shared, batch, &outcome, stamp));
        if ended {
            return (Poll::Ready(()), closes);
        }
        if self.shared.is_cancelled() {
            self.stop();
            closes.append(protocol::finalize(&erased, &self.shared, End::Finished));
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
        let closes: Tokens = match self.node.upgrade() {
            Some(node) => {
                let erased: NodeRef = node;
                protocol::abandon(&erased, &self.shared)
            }
            None => Tokens::new(),
        };
        crate::lifetime::drops::drop_later(state, None);
        closes.close();
    }
}

impl<K: NodeValue> Node<K> {
    /// Publishes an item of `run`, unless the run was superseded (spec §6.4, §7.3 item 4).
    pub(crate) fn publish(&self, run: &RunControl, value: K) {
        let header: &Header = &self.header;
        let mut displaced: Displaced<K> = Displaced::default();
        {
            let _write: MutexGuard<'_, ()> = header.write.lock();
            let current: Option<Arc<K>> = self.value.load_full();
            match current {
                Some(current)
                    if !run.is_cancelled() && header.runs.load(Ordering::SeqCst) == run.id() && !header.is_ghost() =>
                {
                    let _ = self.commit(&current, value, Commit::Item(run), &mut displaced);
                }
                _ => displaced.discarded = Some(value),
            }
        }
        displaced.release(header.runtime.as_ref());
    }
}
