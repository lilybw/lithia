// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Loom models of the scheduling protocol over the validation cases (spec §11.4 step 8).
//! `RUSTFLAGS="--cfg lithia_loom" cargo test -p lithia-test --test loom --release`; bound: `LOOM_MAX_PREEMPTIONS`.

#![cfg(lithia_loom)]
#![allow(clippy::type_complexity)]

mod cases;
mod creation;
mod keyed;
mod lifetime;
mod propagation;
mod wakes;

use lithia::__private::model::{self, ModelScope};
use lithia::{DerivedNode, SourceNode, Status, effect};
use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, Ordering};

/// Silences panics the models cause on purpose, which the run wrapper catches.
fn quiet_expected_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let default: Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static> = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info: &std::panic::PanicHookInfo<'_>| {
            let message: String = info.to_string();
            let expected: bool =
                message.contains("attempt to divide by zero") || message.contains("read of a disposed node");
            if !expected {
                default(info);
            }
        }));
    });
}

/// Explores every interleaving of `f` within the preemption bound; prints how many.
fn check(f: impl Fn() + Sync + Send + 'static) {
    quiet_expected_panics();
    let mut builder: loom::model::Builder = loom::model::Builder::new();
    if builder.preemption_bound.is_none() {
        builder.preemption_bound = Some(3);
    }
    builder.max_branches = 1_000_000;
    let executions: std::sync::Arc<std::sync::atomic::AtomicUsize> = Default::default();
    let counter: std::sync::Arc<std::sync::atomic::AtomicUsize> = executions.clone();
    builder.check(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        model::reset();
        f();
    });
    eprintln!("explored {} interleavings", executions.load(std::sync::atomic::Ordering::Relaxed));
}

/// Runs `f` with one worker thread polling runs next to the model's main thread.
fn with_worker(f: impl FnOnce()) {
    let stop: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let worker: loom::thread::JoinHandle<()> = {
        let stop: Arc<AtomicBool> = stop.clone();
        loom::thread::spawn(move || model::work_until(&stop))
    };
    f();
    stop.store(true, Ordering::SeqCst);
    worker.join().unwrap();
}

/// Spins until `flag` is set, letting loom schedule other threads.
fn wait_for(flag: &AtomicBool) {
    while !flag.load(Ordering::SeqCst) {
        loom::thread::yield_now();
    }
}

/// Suspends once, then completes: an async body's `.await` on something not yet ready.
struct SuspendOnce(bool);

impl std::future::Future for SuspendOnce {
    type Output = ();

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        if self.0 {
            return std::task::Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    }
}

/// Pending until `flag` is set, yielding to loom while it waits.
struct WaitFor(Arc<AtomicBool>);

impl std::future::Future for WaitFor {
    type Output = ();

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        if self.0.load(Ordering::SeqCst) {
            return std::task::Poll::Ready(());
        }
        loom::thread::yield_now();
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    }
}
