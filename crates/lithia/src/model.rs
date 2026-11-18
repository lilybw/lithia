// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! A minimal executor and harness for loom models of the engine (spec §11.4 step 8).
//! Compiled only under `--cfg lithia_loom`; runs are polled on loom threads instead of tokio.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Waker};

use crate::context::{self, Frame, FrameGuard, Target};
use crate::epoch::Scope;
use crate::ids::NodeId;
use crate::node::{NodeRef, Sched};
use crate::sync::MutexGuard;
use crate::{Readable, protocol};

type Task = Pin<Box<dyn Future<Output = ()> + Send>>;

loom::lazy_static! {
    /// Ready runs, as a loom object so the model sees every hand-off between threads.
    static ref QUEUE: crate::sync::Mutex<VecDeque<Task>> = crate::sync::Mutex::new(VecDeque::new());
}

/// Queues a run or a deferred task.
pub(crate) fn spawn(task: Task) {
    QUEUE.lock().push_back(task);
}

/// Clears the queue; loom resets it per execution, so this only guards against misuse.
pub fn reset() {
    let stale: VecDeque<Task> = std::mem::take(&mut *QUEUE.lock());
    drop(stale);
}

/// Polls one queued task; returns whether there was one. Pending tasks go back in the queue.
pub fn run_one() -> bool {
    let task: Option<Task> = QUEUE.lock().pop_front();
    let Some(mut task) = task else {
        return false;
    };
    let mut cx: Context<'_> = Context::from_waker(Waker::noop());
    if task.as_mut().poll(&mut cx).is_pending() {
        spawn(task);
    }
    true
}

fn queue_empty() -> bool {
    QUEUE.lock().is_empty()
}

/// A scope the model's batches are delivered in, like one `#[lithia::test]` task.
#[derive(Clone)]
pub struct ModelScope(Arc<Scope>);

impl ModelScope {
    /// A fresh scope.
    pub fn new() -> ModelScope {
        ModelScope(Scope::new())
    }

    /// Runs `f` as one poll of a lithia task in this scope: its writes form one batch.
    pub fn batch<R>(&self, f: impl FnOnce() -> R) -> R {
        let frame: Frame = Frame::new(Target::Scope(self.0.clone()));
        let result: R = {
            let _entered: FrameGuard<'_> = context::enter(&frame);
            f()
        };
        protocol::deliver_outside(frame.take_batch(), &self.0);
        result
    }

    /// Whether every epoch of the scope has settled.
    pub fn settled(&self) -> bool {
        self.0.is_settled()
    }

    /// Live nodes created in this scope: each node's lifecycle, as the model sees it.
    pub fn live(&self) -> usize {
        self.0.live.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Runs queued work on this thread until the scope has settled, like `flush()`.
    pub fn flush(&self) {
        loop {
            if run_one() {
                continue;
            }
            if self.settled() && queue_empty() {
                return;
            }
            loom::thread::yield_now();
        }
    }
}

impl Default for ModelScope {
    fn default() -> Self {
        ModelScope::new()
    }
}

/// Runs queued work until `stop` is set; the body of a model worker thread.
pub fn work_until(stop: &loom::sync::atomic::AtomicBool) {
    loop {
        if run_one() {
            continue;
        }
        if stop.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        loom::thread::yield_now();
    }
}

/// Whether `node` and its descendants hold no open entry and no run, queued or in flight;
/// checked parents first, so an idle parent has already delivered to its children.
pub fn quiet<N: Readable>(node: &N) -> bool {
    let root: NodeRef = node.__lithia_node().0;
    let mut closure: BTreeMap<NodeId, NodeRef> = BTreeMap::new();
    let mut stack: Vec<NodeRef> = vec![root];
    while let Some(node) = stack.pop() {
        let id: NodeId = node.header().id;
        if closure.contains_key(&id) {
            continue;
        }
        stack.extend(node.header().subscribers());
        closure.insert(id, node);
    }
    closure.values().all(|node: &NodeRef| {
        let guard: MutexGuard<'_, Sched> = node.header().sched.lock();
        !(guard.busy() || guard.unseeded || !guard.entries.is_empty())
    })
}

impl ModelScope {
    /// Runs queued work until the scope has settled and `node`'s subtree is quiet, like
    /// `lithia::test::settled`: it also waits for suspended runs to end.
    pub fn settle<N: Readable>(&self, node: &N) {
        loop {
            if run_one() {
                continue;
            }
            if self.settled() && queue_empty() && quiet(node) {
                return;
            }
            loom::thread::yield_now();
        }
    }
}
