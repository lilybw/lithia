// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! A minimal executor and harness for loom models of the engine (spec §11.4 step 8).
//! Compiled only under `--cfg lithia_loom`; runs are polled on loom threads instead of tokio.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Wake, Waker};

use crate::context::{self, Frame, Target};
use crate::epoch::Scope;
use crate::ids::NodeId;
use crate::node::{NodeRef, SchedGuard};
use crate::{Readable, protocol};

type Task = Pin<Box<dyn Future<Output = ()> + Send>>;
/// Identifies a task while it lives, for its waker.
type TaskId = u64;

/// Tasks by state: ready to poll, parked until woken, or woken while being polled.
#[derive(Default)]
struct Exec {
    ready: VecDeque<(TaskId, Task)>,
    parked: BTreeMap<TaskId, Task>,
    woken: BTreeSet<TaskId>,
    polling: usize,
    next: TaskId,
}

loom::lazy_static! {
    /// The executor's state, as a loom object so the model sees every hand-off between threads.
    static ref EXEC: crate::sync::Mutex<Exec> = crate::sync::Mutex::new(Exec::default());
}

/// Batches and engine operations under way on any thread, which may still wake parked tasks. Plain
/// atomics: bookkeeping for the stall check, not state whose interleavings the model explores.
#[allow(clippy::disallowed_types)]
static BUSY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Notes work under way that may wake a task: a batch, or an engine operation.
pub(crate) fn busy() {
    BUSY.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// Notes the end of work [`busy`] noted.
pub(crate) fn idle() {
    BUSY.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
}

/// Wakes one task: a parked task becomes ready, one being polled is polled again.
struct TaskWaker(TaskId);

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let mut exec: crate::sync::MutexGuard<'_, Exec> = EXEC.lock();
        match exec.parked.remove(&self.0) {
            Some(task) => exec.ready.push_back((self.0, task)),
            None => {
                exec.woken.insert(self.0);
            }
        }
    }
}

/// Queues a run or a deferred task.
pub(crate) fn spawn(task: Task) {
    let mut exec: crate::sync::MutexGuard<'_, Exec> = EXEC.lock();
    let id: TaskId = exec.next;
    exec.next += 1;
    exec.ready.push_back((id, task));
}

/// Clears the executor; loom resets it per execution, so this only guards against misuse.
pub fn reset() {
    let stale: Exec = std::mem::take(&mut *EXEC.lock());
    drop(stale);
    BUSY.store(0, std::sync::atomic::Ordering::SeqCst);
}

/// Polls one ready task with a waker of its own; returns whether there was one. A pending task is
/// parked until woken, so a lost wakeup leaves it parked and the model fails.
pub fn run_one() -> bool {
    let next: Option<(TaskId, Task)> = {
        let mut exec: crate::sync::MutexGuard<'_, Exec> = EXEC.lock();
        let next: Option<(TaskId, Task)> = exec.ready.pop_front();
        if next.is_some() {
            exec.polling += 1;
        }
        next
    };
    let Some((id, mut task)) = next else {
        return false;
    };
    let waker: Waker = Waker::from(Arc::new(TaskWaker(id)));
    let mut cx: Context<'_> = Context::from_waker(&waker);
    let pending: bool = task.as_mut().poll(&mut cx).is_pending();
    let mut exec: crate::sync::MutexGuard<'_, Exec> = EXEC.lock();
    exec.polling -= 1;
    if pending {
        if exec.woken.remove(&id) {
            exec.ready.push_back((id, task));
        } else {
            exec.parked.insert(id, task);
        }
    } else {
        exec.woken.remove(&id);
        drop(exec);
        drop(task);
    }
    true
}

/// Nothing ready and nothing being polled: a task another thread polls may still drop what it holds.
fn queue_empty() -> bool {
    let exec: crate::sync::MutexGuard<'_, Exec> = EXEC.lock();
    exec.ready.is_empty() && exec.polling == 0
}

/// Whether no thread can make progress: nothing ready or being polled, no batch under way, and a
/// task parked. Waiting threads count how often they find it so, and fail the model past a bound.
fn stalled() -> bool {
    let exec: crate::sync::MutexGuard<'_, Exec> = EXEC.lock();
    exec.ready.is_empty()
        && exec.polling == 0
        && !exec.parked.is_empty()
        && BUSY.load(std::sync::atomic::Ordering::SeqCst) == 0
}

/// Idle spins with every task parked after which a waiting thread fails the model.
const STALL_SPINS: u32 = 16;

/// Counts a waiting thread's idle spins; panics once every task has stayed parked too long.
fn note_idle(spins: &mut u32, unsettled: bool) {
    if unsettled && stalled() {
        *spins += 1;
        assert!(
            *spins < STALL_SPINS,
            "lithia model: every task is parked while the scope is unsettled (a lost wakeup)"
        );
    } else {
        *spins = 0;
    }
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
        busy();
        let frame: Frame = Frame::new(Target::Scope(self.0.clone()));
        let result: R = context::entered(&frame, f);
        protocol::deliver_outside(frame.take_batch(), &self.0);
        idle();
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
        let mut spins: u32 = 0;
        loop {
            if run_one() {
                spins = 0;
                continue;
            }
            if self.settled() && queue_empty() {
                return;
            }
            note_idle(&mut spins, !self.settled());
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
        let guard: SchedGuard<'_> = node.header().lock_sched();
        !(guard.busy() || guard.unseeded || !guard.entries.is_empty())
    })
}

impl ModelScope {
    /// Runs queued work until the scope has settled and `node`'s subtree is quiet, like
    /// `lithia::test::settled`: it also waits for suspended runs to end.
    pub fn settle<N: Readable>(&self, node: &N) {
        let mut spins: u32 = 0;
        loop {
            if run_one() {
                spins = 0;
                continue;
            }
            if self.settled() && queue_empty() && quiet(node) {
                return;
            }
            note_idle(&mut spins, true);
            loom::thread::yield_now();
        }
    }
}
