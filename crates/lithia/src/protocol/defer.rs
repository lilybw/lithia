// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Deferred functions: attached to the epochs they wait for, fired once the last settles (spec §6.3).

use super::*;

/// Deferred functions shared by the epochs they wait for; the last epoch to settle fires them.
pub(super) type SharedDefers = Arc<Mutex<Option<Vec<Deferred>>>>;

/// Runs the deferred functions once every one of `epochs` has settled (spec §6.3).
pub(crate) fn attach_defers(epochs: Epochs, defers: Vec<Deferred>) {
    if epochs.is_empty() {
        for deferred in defers {
            fire_deferred(deferred);
        }
        return;
    }
    let remaining: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(epochs.len()));
    let pending: SharedDefers = Arc::new(Mutex::new(Some(defers)));
    for epoch in epochs {
        let remaining: Arc<AtomicUsize> = remaining.clone();
        let pending: SharedDefers = pending.clone();
        epoch.on_settle(Box::new(move || {
            let before: usize = remaining.fetch_sub(1, Ordering::SeqCst);
            debug_assert!(before > 0, "lithia: more epochs settled than deferred functions waited for");
            if before == 1 {
                let list: Option<Vec<Deferred>> = pending.lock().take();
                for deferred in list.into_iter().flatten() {
                    fire_deferred(deferred);
                }
            }
        }));
    }
}

/// Starts a deferred function as a lithia task in its scope, with an empty writer set.
pub(crate) fn fire_deferred(deferred: Deferred) {
    let Deferred { f, token, runtime } = deferred;
    let task_scope: Arc<Scope> = token.scope().clone();
    let future = async move {
        // Returned once the function's writes are delivered, or when the task is dropped unrun.
        let _token: ScopeToken = token;
        TaskFuture::new(task_scope, async move { run_deferred(f) }).await;
    };
    crate::exec::spawn::spawn(runtime.as_ref(), future, crate::exec::spawn::Orphan::DeferThread);
}

/// Runs a deferred function; a panic is reported and its writes up to the panic still land.
pub(super) fn run_deferred(f: Box<dyn FnOnce() + Send>) {
    if let Err(panic) = catch_unwind(AssertUnwindSafe(f)) {
        diag::defer_panicked(&crate::run::panic_message(&*panic));
    }
}
