// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Deferred drops: values and bodies never drop inside the scheduling protocol (spec §10.3).

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
#[cfg(not(lithia_loom))]
use std::sync::LazyLock;
#[cfg(not(lithia_loom))]
use std::sync::mpsc;

use tokio::runtime::Handle;

use crate::node::NodeRef;

crate::sync::thread_local! {
    static RELEASING: Cell<bool> = Cell::new(false);
    static PENDING_PARENTS: RefCell<Vec<NodeRef>> = RefCell::new(Vec::new());
}

/// Payload drops queued on a pool or the drop thread and not yet run.
static QUEUED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Number of payload drops queued and not yet run.
pub(crate) fn queued() -> usize {
    QUEUED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Drops a node value later, unless dropping it can run no user code.
pub(crate) fn drop_value<K: Send + Sync + 'static>(value: Arc<K>, runtime: Option<&Handle>) {
    if !std::mem::needs_drop::<K>() {
        drop(value);
        return;
    }
    drop_later(value, runtime);
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

#[cfg(not(lithia_loom))]
fn drop_on_pool<T: Send + 'static>(payload: T, runtime: Option<&Handle>) {
    QUEUED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let parcel: Parcel = Parcel { payload: Some(Box::new(payload)) };
    match runtime {
        Some(handle) => {
            drop(handle.spawn_blocking(move || parcel.run()));
        }
        None => send_to_drop_thread(parcel),
    }
}

/// A payload on its way to be dropped; if the runtime never runs it, it goes to the drop thread.
#[cfg(not(lithia_loom))]
struct Parcel {
    payload: Option<Box<dyn Any + Send>>,
}

#[cfg(not(lithia_loom))]
impl Parcel {
    fn run(mut self) {
        let payload: Option<Box<dyn Any + Send>> = self.payload.take();
        run_drop(payload);
        QUEUED.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(not(lithia_loom))]
impl Drop for Parcel {
    fn drop(&mut self) {
        if let Some(payload) = self.payload.take() {
            send_to_drop_thread(Parcel { payload: Some(payload) });
        }
    }
}

fn run_drop(payload: Option<Box<dyn Any + Send>>) {
    let outcome: Result<(), Box<dyn Any + Send>> = catch_unwind(AssertUnwindSafe(move || drop(payload)));
    if let Err(panic) = outcome {
        crate::diag::drop_panicked(&crate::run::panic_message(&*panic));
    }
}

/// lithia's drop thread, started on first use.
#[cfg(not(lithia_loom))]
static DROP_THREAD: LazyLock<mpsc::Sender<Parcel>> = LazyLock::new(|| {
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
    if let Err(mpsc::SendError(mut parcel)) = DROP_THREAD.send(parcel) {
        let payload: Option<Box<dyn Any + Send>> = parcel.payload.take();
        run_drop(payload);
        QUEUED.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Drops a freed node's parents iteratively, so a long chain cannot overflow the stack.
pub(crate) fn release_parents(parents: Vec<NodeRef>) {
    let mut parents: Option<Vec<NodeRef>> = Some(parents);
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
