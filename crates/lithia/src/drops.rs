// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Deferred drops: values and bodies never drop inside the scheduling protocol (spec §10.3).

use std::any::{Any, TypeId};
use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
#[cfg(not(lithia_loom))]
use std::sync::mpsc;

use tokio::runtime::Handle;

use crate::node::{NodeRef, Parents};
use crate::run::PanicPayload;

crate::sync::thread_local! {
    static RELEASING: Cell<bool> = const { Cell::new(false) };
    static PENDING_PARENTS: RefCell<Vec<NodeRef>> = const { RefCell::new(Vec::new()) };
}

/// Payload drops queued on a pool or the drop thread and not yet run.
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

/// Drops `payload` on the runtime's blocking pool, or on lithia's drop thread.
pub(crate) fn drop_later<T: Send + 'static>(payload: T, runtime: Option<&Handle>) {
    // Loom models have no blocking pool: drops run inline, still outside every lock.
    #[cfg(lithia_loom)]
    {
        let _ = runtime;
        run_drop(Some(Box::new(payload)));
    }
    #[cfg(not(lithia_loom))]
    drop_on_pool(payload, runtime);
}

/// A payload bound for a runtime's blocking pool, or for the drop thread without one.
#[cfg(not(lithia_loom))]
type Pending = (Option<Handle>, Payload);

/// A value or body on its way to be dropped, type-erased.
type Payload = Box<dyn Any + Send>;

/// Deferred drops gathered on this thread while a batch is open.
#[cfg(not(lithia_loom))]
struct Bag {
    depth: u32,
    pending: Vec<Pending>,
}

#[cfg(not(lithia_loom))]
std::thread_local! {
    static BAG: RefCell<Bag> = const { RefCell::new(Bag { depth: 0, pending: Vec::new() }) };
}

/// Most payloads a batch holds before handing them on, so a long step keeps its garbage bounded.
#[cfg(not(lithia_loom))]
const BAG_LIMIT: usize = 64;

/// Gathers this thread's deferred drops while alive, then queues them as one task per runtime;
/// counts on this thread, so it is not `Send`.
pub(crate) struct Batch {
    #[cfg(not(lithia_loom))]
    counted: bool,
    _thread: std::marker::PhantomData<*const ()>,
}

/// Opens a drop batch on this thread; batches nest, and the outermost one hands the drops on.
pub(crate) fn batch() -> Batch {
    #[cfg(not(lithia_loom))]
    return Batch {
        counted: BAG.try_with(|bag: &RefCell<Bag>| bag.borrow_mut().depth += 1).is_ok(),
        _thread: std::marker::PhantomData,
    };
    #[cfg(lithia_loom)]
    return Batch { _thread: std::marker::PhantomData };
}

impl Drop for Batch {
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
        dispatch(runtime.cloned(), Parcel { payloads: vec![payload] });
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
    let mut parcels: Parcels = Vec::new();
    for (runtime, payload) in pending {
        let id: Option<tokio::runtime::Id> = runtime.as_ref().map(Handle::id);
        match parcels.iter_mut().find(|(known, _): &&mut (Option<Handle>, Parcel)| known.as_ref().map(Handle::id) == id)
        {
            Some((_, parcel)) => parcel.payloads.push(payload),
            None => parcels.push((runtime, Parcel { payloads: vec![payload] })),
        }
    }
    for (runtime, parcel) in parcels {
        dispatch(runtime, parcel);
    }
}

#[cfg(not(lithia_loom))]
fn dispatch(runtime: Option<Handle>, parcel: Parcel) {
    match runtime {
        Some(handle) => {
            drop(handle.spawn_blocking(move || parcel.run()));
        }
        None => send_to_drop_thread(parcel),
    }
}

/// Payloads on their way to be dropped, each under its own `catch_unwind`; if the runtime never
/// runs them, they go to the drop thread.
#[cfg(not(lithia_loom))]
struct Parcel {
    payloads: Vec<Payload>,
}

#[cfg(not(lithia_loom))]
impl Parcel {
    fn run(mut self) {
        // What these drops free in turn, such as nodes whose last handle a value held, goes together.
        let _drops: Batch = batch();
        for payload in std::mem::take(&mut self.payloads) {
            run_drop(Some(payload));
            let before: usize = QUEUED.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            debug_assert!(before > 0, "lithia: more deferred drops ran than were queued");
        }
    }
}

#[cfg(not(lithia_loom))]
impl Drop for Parcel {
    fn drop(&mut self) {
        if !self.payloads.is_empty() {
            send_to_drop_thread(Parcel { payloads: std::mem::take(&mut self.payloads) });
        }
    }
}

fn run_drop(payload: Option<Payload>) {
    let outcome: Result<(), PanicPayload> = catch_unwind(AssertUnwindSafe(move || drop(payload)));
    if let Err(panic) = outcome {
        crate::diag::drop_panicked(&crate::run::panic_message(&*panic));
    }
}

/// lithia's drop thread, started on first use.
#[cfg(not(lithia_loom))]
#[allow(clippy::disallowed_types)] // lithia's own thread, absent from loom builds
static DROP_THREAD: std::sync::LazyLock<mpsc::Sender<Parcel>> = std::sync::LazyLock::new(|| {
    let (sender, receiver): (mpsc::Sender<Parcel>, mpsc::Receiver<Parcel>) = mpsc::channel();
    std::thread::Builder::new()
        .name("lithia-drop".to_string())
        .spawn(move || {
            while let Ok(parcel) = receiver.recv() {
                parcel.run();
            }
        })
        .expect("lithia: cannot start the drop thread");
    sender
});

#[cfg(not(lithia_loom))]
fn send_to_drop_thread(parcel: Parcel) {
    if let Err(mpsc::SendError(parcel)) = DROP_THREAD.send(parcel) {
        parcel.run();
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
