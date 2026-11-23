// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! The fast path (spec §11.4, step 4): a run task runs the next ready synchronous run of its own
//! runtime itself, in a loop; a step that readies several packs them into runner tasks.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::epoch::Tokens;
use crate::node::{NodeRef, WeakNodeRef};

/// Ready synchronous runs grouped by runtime, to pack into runner tasks.
type Packs = Vec<(RuntimeKey, Vec<NodeRef>)>;

/// One run, type-erased, to poll here or to spawn.
pub(crate) type RunTask = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Most runs one task runs back to back before it hands the rest on.
const RUNS: u32 = 32;

/// Most runs packed into one runner task.
const PACK: usize = 16;

/// How long one task runs back to back; past it, the runs left get tasks of their own.
#[cfg(not(lithia_loom))]
const SLICE: std::time::Duration = std::time::Duration::from_micros(50);

/// The runtime a run belongs to, as the fast path compares them.
#[cfg(not(lithia_loom))]
pub(crate) type RuntimeKey = Option<tokio::runtime::Id>;
/// Loom models run on one model executor, so every run belongs to it.
#[cfg(lithia_loom)]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RuntimeKey;

/// The fast path of the task polled on this thread.
#[derive(Default)]
struct Inline {
    /// Whether a run or runner task on this thread runs further runs itself.
    open: bool,
    runtime: RuntimeKey,
    /// Ready synchronous runs of this runtime its steps handed it, to run next here.
    next: VecDeque<WeakNodeRef>,
    /// Token closes of the runs run here, closed together when the task's poll ends.
    closes: Tokens,
}

impl Inline {
    const CLOSED: Inline = Inline { open: false, runtime: NO_RUNTIME, next: VecDeque::new(), closes: Tokens::new() };
}

crate::sync::thread_local! {
    static INLINE: RefCell<Inline> = const { RefCell::new(Inline::CLOSED) };
}

/// The fast path, open while a run or runner task is polled; closes when dropped.
pub(crate) struct Open(());

/// Opens the fast path for the task polled on this thread, which runs on `runtime`; `None`
/// while one already is.
pub(crate) fn open(runtime: RuntimeKey) -> Option<Open> {
    INLINE
        .try_with(|cell: &RefCell<Inline>| {
            let mut inline: std::cell::RefMut<'_, Inline> = cell.borrow_mut();
            if inline.open {
                return None;
            }
            inline.open = true;
            inline.runtime = runtime;
            Some(Open(()))
        })
        .ok()
        .flatten()
}

impl Open {
    /// Runs the runs handed to this thread, then those in `queue`, back to back within the budget,
    /// and hands the rest on. Each is its own run, polled here: a loop, never recursion.
    pub(crate) fn run(&self, cx: &mut Context<'_>, queue: &mut VecDeque<WeakNodeRef>) {
        // The clock is read only once something runs here.
        #[cfg(not(lithia_loom))]
        let mut start: Option<std::time::Instant> = None;
        let mut ran: u32 = 0;
        loop {
            let Some(weak) = take_next().or_else(|| queue.pop_front()) else {
                break;
            };
            let Some(node) = weak.upgrade() else {
                continue;
            };
            #[cfg(not(lithia_loom))]
            let heavy: bool = start.get_or_insert_with(std::time::Instant::now).elapsed() >= SLICE && ran > 0;
            #[cfg(lithia_loom)]
            let heavy: bool = false;
            if heavy || ran >= RUNS {
                hand_on(node, queue, heavy);
                break;
            }
            ran += 1;
            let mut task: RunTask = node.clone().make_run();
            if task.as_mut().poll(cx).is_pending() {
                spawn_task(node.header().runtime.clone(), task);
            }
            drop(node);
        }
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        let (next, closes): (VecDeque<WeakNodeRef>, Tokens) = INLINE
            .try_with(|cell: &RefCell<Inline>| {
                let mut inline: std::cell::RefMut<'_, Inline> = cell.borrow_mut();
                inline.open = false;
                (std::mem::take(&mut inline.next), std::mem::take(&mut inline.closes))
            })
            .unwrap_or_default();
        // Runs still handed here, as after a panic, get tasks of their own.
        for weak in next {
            if let Some(node) = weak.upgrade() {
                node.spawn_run();
            }
        }
        closes.close();
    }
}

/// The fast path closed while user code polls, so a body that blocks until runs finish never waits
/// for runs only its own poll's end would start; reopened as it was when dropped.
pub(crate) struct Paused {
    saved: Option<Inline>,
    _thread: std::marker::PhantomData<*const ()>,
}

/// Closes the fast path of this thread, if open, until the guard drops.
pub(crate) fn pause() -> Paused {
    let saved: Option<Inline> = INLINE
        .try_with(|cell: &RefCell<Inline>| {
            let mut inline: std::cell::RefMut<'_, Inline> = cell.borrow_mut();
            inline.open.then(|| std::mem::take(&mut *inline))
        })
        .ok()
        .flatten();
    Paused { saved, _thread: std::marker::PhantomData }
}

impl Drop for Paused {
    fn drop(&mut self) {
        if let Some(saved) = self.saved.take() {
            let _ = INLINE.try_with(|cell: &RefCell<Inline>| {
                let mut inline: std::cell::RefMut<'_, Inline> = cell.borrow_mut();
                debug_assert!(
                    !inline.open && inline.next.is_empty() && inline.closes.is_empty(),
                    "lithia: the fast path was left open or holding runs while paused"
                );
                *inline = saved;
            });
        }
    }
}

/// Closes a run's tokens when the task's poll ends if it runs on the fast path, else now; its
/// node's references are gone either way (spec §10.1).
pub(crate) fn close_later(closes: Tokens) {
    let mut closes: Option<Tokens> = Some(closes);
    let _ = INLINE.try_with(|cell: &RefCell<Inline>| {
        let mut inline: std::cell::RefMut<'_, Inline> = cell.borrow_mut();
        if inline.open
            && let Some(more) = closes.take()
        {
            inline.closes.append(more);
        }
    });
    if let Some(closes) = closes {
        closes.close();
    }
}

/// Hands a step's ready runs on: synchronous ones run here or in runner tasks of their runtime,
/// any other run in a task of its own (spec §11.4, step 4).
pub(crate) fn dispatch(mut spawns: Vec<NodeRef>) {
    if spawns.len() == 1 {
        let node: NodeRef = spawns.remove(0);
        if !(node.header().sync_body && keep(std::slice::from_ref(&node))) {
            node.spawn_run();
        }
        return;
    }
    let mut packs: Packs = Vec::new();
    for node in spawns {
        if !node.header().sync_body {
            node.spawn_run();
            continue;
        }
        let key: RuntimeKey = runtime_of(&node);
        match packs.iter_mut().find(|(known, _): &&mut (RuntimeKey, Vec<NodeRef>)| *known == key) {
            Some((_, nodes)) => nodes.push(node),
            None => packs.push((key, vec![node])),
        }
    }
    for (_, nodes) in packs {
        pack(nodes);
    }
}

/// Takes synchronous runs of one runtime to run next on this thread, if its fast path is open
/// for that runtime and has room for them.
fn keep(nodes: &[NodeRef]) -> bool {
    enabled()
        && !nodes.is_empty()
        && INLINE
            .try_with(|cell: &RefCell<Inline>| {
                let mut inline: std::cell::RefMut<'_, Inline> = cell.borrow_mut();
                let fits: bool = inline.open
                    && inline.next.len() + nodes.len() <= PACK
                    && same_runtime(inline.runtime, runtime_of(&nodes[0]));
                if fits {
                    inline.next.extend(nodes.iter().map(Arc::downgrade));
                }
                fits
            })
            .unwrap_or(false)
}

fn take_next() -> Option<WeakNodeRef> {
    INLINE.try_with(|cell: &RefCell<Inline>| cell.borrow_mut().next.pop_front()).ok().flatten()
}

/// Packs synchronous runs of one runtime into runner tasks, about one per worker and at most
/// `PACK` each; a task whose fast path is open for that runtime keeps the first share itself.
fn pack(mut nodes: Vec<NodeRef>) {
    let size: usize = nodes.len().div_ceil(workers(&nodes[0])).clamp(1, PACK);
    let share: usize = size.min(nodes.len());
    if keep(&nodes[..share]) {
        nodes.drain(..share);
    }
    if nodes.is_empty() {
        return;
    }
    if size == 1 || !enabled() {
        for node in nodes {
            node.spawn_run();
        }
        return;
    }
    let runtime: Option<tokio::runtime::Handle> = nodes[0].header().runtime.clone();
    let key: RuntimeKey = runtime_of(&nodes[0]);
    let mut queue: VecDeque<WeakNodeRef> = VecDeque::with_capacity(size);
    for node in nodes {
        queue.push_back(Arc::downgrade(&node));
        drop(node);
        if queue.len() == size {
            spawn_task(runtime.clone(), Box::pin(Runner { runtime: key, queue: std::mem::take(&mut queue) }));
        }
    }
    if !queue.is_empty() {
        spawn_task(runtime, Box::pin(Runner { runtime: key, queue }));
    }
}

/// Hands on the runs left once the budget is spent: after heavy runs, each gets a task of its own;
/// after many cheap ones, they go on together in one runner task.
fn hand_on(node: NodeRef, queue: &mut VecDeque<WeakNodeRef>, heavy: bool) {
    let runtime: Option<tokio::runtime::Handle> = node.header().runtime.clone();
    let key: RuntimeKey = runtime_of(&node);
    let mut left: VecDeque<WeakNodeRef> = VecDeque::with_capacity(queue.len() + 2);
    left.push_back(Arc::downgrade(&node));
    drop(node);
    while let Some(weak) = take_next() {
        left.push_back(weak);
    }
    left.extend(queue.drain(..));
    if heavy || left.len() == 1 {
        for weak in left {
            if let Some(node) = weak.upgrade() {
                node.spawn_run();
            }
        }
    } else {
        spawn_task(runtime, Box::pin(Runner { runtime: key, queue: left }));
    }
}

/// Ready synchronous runs of one runtime, run back to back as one task.
struct Runner {
    runtime: RuntimeKey,
    queue: VecDeque<WeakNodeRef>,
}

impl Future for Runner {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this: &mut Runner = self.get_mut();
        // Its runs' deferred drops are queued together when the poll ends (spec §10.3).
        let _drops: crate::drops::Batch = crate::drops::batch();
        match open(this.runtime) {
            Some(open) => open.run(cx, &mut this.queue),
            None => {
                for weak in this.queue.drain(..) {
                    if let Some(node) = weak.upgrade() {
                        node.spawn_run();
                    }
                }
            }
        }
        Poll::Ready(())
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        // Dropped before its poll, as by a runtime shutting down: each run gets a task of its
        // own, which then ends that run as a shutdown does (spec §11.3).
        for weak in self.queue.drain(..) {
            if let Some(node) = weak.upgrade() {
                node.spawn_run();
            }
        }
    }
}

fn spawn_task(runtime: Option<tokio::runtime::Handle>, task: RunTask) {
    #[cfg(lithia_loom)]
    {
        let _ = runtime;
        crate::model::spawn(task);
    }
    #[cfg(not(lithia_loom))]
    match runtime {
        Some(handle) => drop(handle.spawn(task)),
        None => drop(task),
    }
}

/// Loom models take the fast path unless `LITHIA_LOOM_FAST_PATH=off`, so CI checks both paths.
#[cfg(lithia_loom)]
fn enabled() -> bool {
    #[allow(clippy::disallowed_types)] // a model's configuration, read once
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("LITHIA_LOOM_FAST_PATH").map_or(true, |value: String| value != "off"))
}

#[cfg(not(lithia_loom))]
fn enabled() -> bool {
    true
}

/// The key of a run whose node is gone, which never takes the fast path.
#[cfg(not(lithia_loom))]
const NO_RUNTIME: RuntimeKey = None;
#[cfg(lithia_loom)]
const NO_RUNTIME: RuntimeKey = RuntimeKey;

pub(crate) fn no_runtime() -> RuntimeKey {
    NO_RUNTIME
}

#[cfg(not(lithia_loom))]
pub(crate) fn runtime_of(node: &NodeRef) -> RuntimeKey {
    node.header().runtime.as_ref().map(tokio::runtime::Handle::id)
}

#[cfg(lithia_loom)]
pub(crate) fn runtime_of(_: &NodeRef) -> RuntimeKey {
    RuntimeKey
}

#[cfg(not(lithia_loom))]
fn same_runtime(open: RuntimeKey, node: RuntimeKey) -> bool {
    open.is_some() && open == node
}

#[cfg(lithia_loom)]
fn same_runtime(_: RuntimeKey, _: RuntimeKey) -> bool {
    true
}

/// Worker threads of the node's runtime; loom models count two, so small fan-outs keep a task each.
#[cfg(not(lithia_loom))]
fn workers(node: &NodeRef) -> usize {
    node.header().runtime.as_ref().map_or(1, |handle: &tokio::runtime::Handle| handle.metrics().num_workers().max(1))
}

#[cfg(lithia_loom)]
fn workers(_: &NodeRef) -> usize {
    2
}
