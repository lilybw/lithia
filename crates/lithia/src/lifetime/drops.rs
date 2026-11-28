// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Deferred drops: values and bodies never drop inside the scheduling protocol (spec §10.3).

use std::any::{Any, TypeId};
use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use tokio::runtime::Handle;

use crate::graph::node::{NodeRef, Parents};
use crate::run::PanicPayload;

crate::sync::thread_local! {
    static RELEASING: Cell<bool> = const { Cell::new(false) };
    static PENDING_PARENTS: RefCell<Vec<NodeRef>> = const { RefCell::new(Vec::new()) };
}

/// Payload drops queued on the drop threads and not yet run.
#[allow(clippy::disallowed_types)] // counts for test support; loom models drop inline
static QUEUED: crate::sync::Padded<std::sync::atomic::AtomicUsize> =
    crate::sync::Padded(std::sync::atomic::AtomicUsize::new(0));

/// Number of payload drops queued and not yet run.
pub(crate) fn queued() -> usize {
    QUEUED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Drops a node value later, unless dropping it can run no user code.
pub(crate) fn drop_value<K: Send + Sync + 'static>(value: Arc<K>, runtime: Option<&Handle>) {
    if drops_inline::<K>() {
        drop(value);
        return;
    }
    drop_later(value, runtime);
}

/// Drops a value no node holds (discarded by `equals` or a superseded run) like `drop_value`.
pub(crate) fn drop_owned<K: Send + 'static>(value: K, runtime: Option<&Handle>) {
    if drops_inline::<K>() {
        drop(value);
        return;
    }
    drop_later(value, runtime);
}

/// Whether dropping a `K` can run no user code: no drop glue, or a string or byte buffer.
fn drops_inline<K: 'static>() -> bool {
    if !std::mem::needs_drop::<K>() {
        return true;
    }
    let id: TypeId = TypeId::of::<K>();
    [
        TypeId::of::<String>(),
        TypeId::of::<Box<str>>(),
        TypeId::of::<Arc<str>>(),
        TypeId::of::<Vec<u8>>(),
        TypeId::of::<Box<[u8]>>(),
        TypeId::of::<Arc<[u8]>>(),
    ]
    .contains(&id)
}

/// Drops `payload` on lithia's drop threads, in its node's runtime context.
pub(crate) fn drop_later<T: Send + 'static>(payload: T, runtime: Option<&Handle>) {
    // Loom models have no drop threads: drops run inline, still outside every lock.
    #[cfg(lithia_loom)]
    {
        let _ = runtime;
        run_drop(Some(Box::new(payload)));
    }
    #[cfg(not(lithia_loom))]
    drop_on_pool(payload, runtime);
}

/// A payload and the runtime of the node it came from, if any.
#[cfg(not(lithia_loom))]
type Pending = (Option<Handle>, Payload);

/// A value or body on its way to be dropped, type-erased.
type Payload = Box<dyn Any + Send>;

/// Deferred drops gathered on this thread while a gather is open.
#[cfg(not(lithia_loom))]
struct Bag {
    depth: u32,
    pending: Vec<Pending>,
}

#[cfg(not(lithia_loom))]
std::thread_local! {
    static BAG: RefCell<Bag> = const { RefCell::new(Bag { depth: 0, pending: Vec::new() }) };
}

/// Most payloads a gather holds before handing them on, so a long step keeps its garbage bounded.
#[cfg(not(lithia_loom))]
const BAG_LIMIT: usize = 64;

/// Gathers this thread's deferred drops while alive, then queues them as one task per runtime;
/// counts on this thread, so it is not `Send`.
pub(crate) struct DropBatch {
    #[cfg(not(lithia_loom))]
    counted: bool,
    _thread: std::marker::PhantomData<*const ()>,
}

/// Opens a drop gather on this thread; batches nest, and the outermost one hands the drops on.
pub(crate) fn gather() -> DropBatch {
    #[cfg(not(lithia_loom))]
    return DropBatch {
        counted: BAG.try_with(|bag: &RefCell<Bag>| bag.borrow_mut().depth += 1).is_ok(),
        _thread: std::marker::PhantomData,
    };
    #[cfg(lithia_loom)]
    return DropBatch { _thread: std::marker::PhantomData };
}

impl Drop for DropBatch {
    fn drop(&mut self) {
        #[cfg(not(lithia_loom))]
        if self.counted {
            let pending: Vec<Pending> = BAG
                .try_with(|bag: &RefCell<Bag>| {
                    let mut bag: std::cell::RefMut<'_, Bag> = bag.borrow_mut();
                    bag.depth -= 1;
                    if bag.depth == 0 { std::mem::take(&mut bag.pending) } else { Vec::new() }
                })
                .unwrap_or_default();
            send(pending);
        }
    }
}

#[cfg(not(lithia_loom))]
fn drop_on_pool<T: Send + 'static>(payload: T, runtime: Option<&Handle>) {
    QUEUED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let mut loose: Option<Payload> = Some(Box::new(payload));
    let full: Option<Vec<Pending>> = BAG
        .try_with(|bag: &RefCell<Bag>| {
            let mut bag: std::cell::RefMut<'_, Bag> = bag.borrow_mut();
            if bag.depth == 0 {
                return None;
            }
            let payload: Payload = loose.take()?;
            bag.pending.push((runtime.cloned(), payload));
            (bag.pending.len() >= BAG_LIMIT).then(|| std::mem::take(&mut bag.pending))
        })
        .ok()
        .flatten();
    if let Some(payload) = loose {
        pool::post(Parcel { runtime: runtime.cloned(), payloads: vec![payload] });
    }
    if let Some(full) = full {
        send(full);
    }
}

/// Parcels being gathered, one per runtime.
#[cfg(not(lithia_loom))]
type Parcels = Vec<(Option<Handle>, Parcel)>;

/// Queues gathered payloads as one parcel per runtime.
#[cfg(not(lithia_loom))]
fn send(pending: Vec<Pending>) {
    if pending.is_empty() {
        return;
    }
    let mut parcels: Parcels = Vec::new();
    for (runtime, payload) in pending {
        let id: Option<tokio::runtime::Id> = runtime.as_ref().map(Handle::id);
        match parcels.iter_mut().find(|(known, _): &&mut (Option<Handle>, Parcel)| known.as_ref().map(Handle::id) == id)
        {
            Some((_, parcel)) => parcel.payloads.push(payload),
            None => parcels.push((runtime.clone(), Parcel { runtime, payloads: vec![payload] })),
        }
    }
    for (_, parcel) in parcels {
        pool::post(parcel);
    }
}

/// Payloads of one runtime on their way to be dropped, each under its own `catch_unwind`.
#[cfg(not(lithia_loom))]
struct Parcel {
    runtime: Option<Handle>,
    payloads: Vec<Payload>,
}

#[cfg(not(lithia_loom))]
impl Parcel {
    fn run(self) {
        // In the runtime's context, so a `Drop` may spawn tasks on it (spec §10.3).
        let _context: Option<tokio::runtime::EnterGuard<'_>> = self.runtime.as_ref().map(Handle::enter);
        // What these drops free in turn, such as nodes whose last handle a value held, goes together.
        let _drops: DropBatch = gather();
        for payload in self.payloads {
            run_drop(Some(payload));
            let before: usize = QUEUED.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            debug_assert!(before > 0, "lithia: more deferred drops ran than were queued");
        }
    }
}

fn run_drop(payload: Option<Payload>) {
    let outcome: Result<(), PanicPayload> = catch_unwind(AssertUnwindSafe(move || drop(payload)));
    if let Err(panic) = outcome {
        crate::diag::drop_panicked(&crate::run::panic_message(&*panic));
    }
}

/// lithia's drop threads: one at first, and another whenever the queue has made no progress for
/// `STALL` or outgrows them, so a `Drop` that blocks holds up only the drops queued behind it (spec §10.3).
#[cfg(not(lithia_loom))]
#[allow(clippy::disallowed_types)] // lithia's own threads, absent from loom builds
mod pool {
    use std::collections::VecDeque;
    use std::sync::{Condvar, LazyLock, Mutex, MutexGuard, PoisonError};
    use std::time::Duration;

    use super::Parcel;

    /// How long a queue may wait with no parcel taken before another thread joins.
    const STALL: Duration = Duration::from_millis(10);
    /// How long a drop thread beyond the first waits for work before it ends.
    const KEEP_ALIVE: Duration = Duration::from_secs(10);
    /// Parcels queued per drop thread beyond which another joins.
    const BACKLOG: usize = 4;
    /// Most drop threads at once.
    const MOST: usize = 64;

    struct State {
        queue: VecDeque<Parcel>,
        threads: usize,
        idle: usize,
        /// Parcels taken so far, which the watcher reads as progress.
        taken: u64,
        watching: bool,
        watcher: bool,
    }

    struct Pool {
        state: Mutex<State>,
        work: Condvar,
        watch: Condvar,
    }

    static POOL: LazyLock<Pool> = LazyLock::new(|| Pool {
        state: Mutex::new(State {
            queue: VecDeque::new(),
            threads: 0,
            idle: 0,
            taken: 0,
            watching: false,
            watcher: false,
        }),
        work: Condvar::new(),
        watch: Condvar::new(),
    });

    fn lock() -> MutexGuard<'static, State> {
        POOL.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues `parcel`; wakes an idle thread, or, with none, starts the first or watches the queue.
    pub(super) fn post(parcel: Parcel) {
        let mut state: MutexGuard<'static, State> = lock();
        state.queue.push_back(parcel);
        if state.idle > 0 {
            POOL.work.notify_one();
        } else if state.threads == 0 {
            start_worker(&mut state);
        } else {
            arm(&mut state);
        }
    }

    /// Has the watcher watch the queue, which no idle thread will take from.
    fn arm(state: &mut State) {
        if state.watching {
            return;
        }
        state.watching = true;
        if state.watcher {
            POOL.watch.notify_one();
        } else {
            let started: std::io::Result<std::thread::JoinHandle<()>> =
                std::thread::Builder::new().name("lithia-drop-watch".to_string()).spawn(watch);
            // Without a watcher the running threads carry on; the next backlog tries again.
            state.watcher = started.is_ok();
            state.watching = state.watcher;
        }
    }

    fn start_worker(state: &mut State) {
        match std::thread::Builder::new().name("lithia-drop".to_string()).spawn(work) {
            Ok(_) => state.threads += 1,
            Err(error) if state.threads == 0 => panic!("lithia: cannot start a drop thread: {error}"),
            // The threads running carry on; the watcher tries again after the next stall.
            Err(_) => {}
        }
    }

    fn work() {
        let mut state: MutexGuard<'static, State> = lock();
        loop {
            if let Some(parcel) = state.queue.pop_front() {
                state.taken += 1;
                if !state.queue.is_empty() && state.idle == 0 {
                    arm(&mut state);
                }
                drop(state);
                parcel.run();
                state = lock();
                continue;
            }
            state.idle += 1;
            let waited: std::sync::WaitTimeoutResult;
            (state, waited) = POOL.work.wait_timeout(state, KEEP_ALIVE).unwrap_or_else(PoisonError::into_inner);
            state.idle -= 1;
            if waited.timed_out() && state.queue.is_empty() && state.threads > 1 {
                state.threads -= 1;
                return;
            }
        }
    }

    /// Starts a drop thread whenever a backlog makes no progress for `STALL`, or outgrows the threads.
    fn watch() {
        let mut state: MutexGuard<'static, State> = lock();
        loop {
            while !state.watching {
                state = POOL.watch.wait(state).unwrap_or_else(PoisonError::into_inner);
            }
            let taken: u64 = state.taken;
            (state, _) = POOL.watch.wait_timeout(state, STALL).unwrap_or_else(PoisonError::into_inner);
            if state.queue.is_empty() {
                state.watching = false;
                continue;
            }
            let stuck: bool = state.taken == taken && state.idle == 0;
            let behind: bool = state.queue.len() > state.threads * BACKLOG;
            if (stuck || behind) && state.threads < MOST {
                start_worker(&mut state);
            }
        }
    }
}

/// Drops a freed node's parents iteratively, so a long chain cannot overflow the stack.
pub(crate) fn release_parents(parents: Parents) {
    let mut parents: Option<Parents> = Some(parents);
    let queued: bool = PENDING_PARENTS
        .try_with(|pending: &RefCell<Vec<NodeRef>>| pending.borrow_mut().extend(parents.take().into_iter().flatten()))
        .is_ok();
    if !queued {
        drop(parents);
        return;
    }
    if RELEASING.try_with(|flag: &Cell<bool>| flag.replace(true)).unwrap_or(true) {
        return;
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            let _ = RELEASING.try_with(|flag: &Cell<bool>| flag.set(false));
        }
    }
    let _reset: Reset = Reset;
    loop {
        let next: Option<NodeRef> =
            PENDING_PARENTS.try_with(|pending: &RefCell<Vec<NodeRef>>| pending.borrow_mut().pop()).ok().flatten();
        match next {
            Some(node) => drop(node),
            None => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_types_drop_inline() {
        assert!(drops_inline::<u64>() && drops_inline::<()>() && drops_inline::<(u8, f64)>());
        assert!(drops_inline::<String>() && drops_inline::<Box<str>>() && drops_inline::<Arc<str>>());
        assert!(drops_inline::<Vec<u8>>() && drops_inline::<Box<[u8]>>() && drops_inline::<Arc<[u8]>>());
        assert!(!drops_inline::<Vec<String>>() && !drops_inline::<Option<String>>());
        assert!(!drops_inline::<Arc<dyn Fn() + Send + Sync>>() && !drops_inline::<Box<dyn Any + Send>>());
    }
}
