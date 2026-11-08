//! Deferred drops: values and bodies never drop inside the scheduling protocol (spec §10.3).

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
#[cfg(not(lithia_loom))]
use std::sync::OnceLock;
#[cfg(not(lithia_loom))]
use std::sync::mpsc;

use tokio::runtime::Handle;

use crate::node::NodeRef;

crate::sync::thread_local! {
    static IN_VALUE_DROP: Cell<bool> = Cell::new(false);
    static RELEASING: Cell<bool> = Cell::new(false);
    static PENDING_PARENTS: RefCell<Vec<NodeRef>> = RefCell::new(Vec::new());
}

/// Whether this thread is dropping a replaced, discarded or freed value.
pub(crate) fn in_value_drop() -> bool {
    IN_VALUE_DROP.try_with(Cell::get).unwrap_or(false)
}

/// Drops a node value later, unless dropping it can run no user code.
pub(crate) fn drop_value<K: Send + Sync + 'static>(value: Arc<K>, runtime: Option<&Handle>) {
    if !std::mem::needs_drop::<K>() {
        drop(value);
        return;
    }
    drop_later(value, runtime, true);
}

/// Drops `payload` on the runtime's blocking pool, or on lithia's drop thread.
pub(crate) fn drop_later<T: Send + 'static>(payload: T, runtime: Option<&Handle>, is_value: bool) {
    // Loom models have no blocking pool: drops run inline, still outside every lock.
    #[cfg(lithia_loom)]
    {
        let _ = runtime;
        run_drop(Some(Box::new(payload)), is_value);
    }
    #[cfg(not(lithia_loom))]
    drop_on_pool(payload, runtime, is_value);
}

#[cfg(not(lithia_loom))]
fn drop_on_pool<T: Send + 'static>(payload: T, runtime: Option<&Handle>, is_value: bool) {
    let parcel: Parcel = Parcel { payload: Some(Box::new(payload)), is_value };
    match runtime {
        Some(handle) => {
            drop(handle.spawn_blocking(move || parcel.run()));
        }
        None => send_to_drop_thread(parcel),
    }
}

#[cfg(not(lithia_loom))]
/// A payload on its way to be dropped; if the runtime never runs it, it goes to the drop thread.
struct Parcel {
    payload: Option<Box<dyn Any + Send>>,
    is_value: bool,
}

#[cfg(not(lithia_loom))]
impl Parcel {
    fn run(mut self) {
        let payload: Option<Box<dyn Any + Send>> = self.payload.take();
        run_drop(payload, self.is_value);
    }
}

#[cfg(not(lithia_loom))]
impl Drop for Parcel {
    fn drop(&mut self) {
        if let Some(payload) = self.payload.take() {
            send_to_drop_thread(Parcel { payload: Some(payload), is_value: self.is_value });
        }
    }
}

fn run_drop(payload: Option<Box<dyn Any + Send>>, is_value: bool) {
    let previous: bool = IN_VALUE_DROP.try_with(|flag: &Cell<bool>| flag.replace(is_value)).unwrap_or(false);
    let outcome: Result<(), Box<dyn Any + Send>> = catch_unwind(AssertUnwindSafe(move || drop(payload)));
    let _ = IN_VALUE_DROP.try_with(|flag: &Cell<bool>| flag.set(previous));
    if let Err(panic) = outcome {
        crate::diag::drop_panicked(&crate::run::panic_message(&*panic));
    }
}

#[cfg(not(lithia_loom))]
fn drop_thread() -> &'static mpsc::Sender<Parcel> {
    static SENDER: OnceLock<mpsc::Sender<Parcel>> = OnceLock::new();
    SENDER.get_or_init(|| {
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
    })
}

#[cfg(not(lithia_loom))]
fn send_to_drop_thread(parcel: Parcel) {
    if let Err(mpsc::SendError(mut parcel)) = drop_thread().send(parcel) {
        let payload: Option<Box<dyn Any + Send>> = parcel.payload.take();
        run_drop(payload, parcel.is_value);
    }
}

/// Drops a freed node's parents iteratively, so a long chain cannot overflow the stack.
pub(crate) fn release_parents(parents: Vec<NodeRef>) {
    let queued: bool = PENDING_PARENTS
        .try_with(|pending: &RefCell<Vec<NodeRef>>| pending.borrow_mut().extend(parents.iter().cloned()))
        .is_ok();
    if !queued {
        drop(parents);
        return;
    }
    drop(parents);
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
