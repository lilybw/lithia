// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Spawning lithia's tasks: on a runtime, on loom's model executor, or on lithia's defer thread.

use std::future::Future;

use tokio::runtime::Handle;

/// What becomes of a task when no runtime is at hand.
pub(crate) enum Orphan {
    /// It is dropped unpolled: a run whose node has no runtime any more.
    Drop,
    /// It runs on lithia's defer thread: a deferred function fired outside any runtime.
    DeferThread,
}

/// Spawns `task` on `runtime`, or on the model executor under loom; with no runtime, `orphan` says where.
/// `spawn(Some(&handle), async {}, Orphan::Drop);`
pub(crate) fn spawn<F: Future<Output = ()> + Send + 'static>(runtime: Option<&Handle>, task: F, orphan: Orphan) {
    #[cfg(lithia_loom)]
    {
        let _ = (runtime, orphan);
        crate::exec::model::spawn(Box::pin(task));
    }
    #[cfg(not(lithia_loom))]
    match (runtime, orphan) {
        (Some(handle), _) => drop(handle.spawn(task)),
        (None, Orphan::Drop) => drop(task),
        (None, Orphan::DeferThread) => {
            let _ = DEFER_THREAD.send(Box::pin(task));
        }
    }
}

/// A deferred function's task, for the defer thread.
#[cfg(not(lithia_loom))]
type DeferTask = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

/// lithia's defer thread, started on first use: a single-threaded runtime for deferred functions
/// fired where no runtime runs.
#[cfg(not(lithia_loom))]
#[allow(clippy::disallowed_types)] // lithia's own thread, absent from loom builds
static DEFER_THREAD: std::sync::LazyLock<tokio::sync::mpsc::UnboundedSender<DeferTask>> =
    std::sync::LazyLock::new(|| {
        let (sender, mut receiver): (
            tokio::sync::mpsc::UnboundedSender<DeferTask>,
            tokio::sync::mpsc::UnboundedReceiver<DeferTask>,
        ) = tokio::sync::mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("lithia-defer".to_string())
            .spawn(move || {
                let runtime: tokio::runtime::Runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("lithia: cannot build the defer thread's runtime");
                runtime.block_on(async move {
                    while let Some(task) = receiver.recv().await {
                        drop(tokio::spawn(task));
                    }
                });
            })
            .expect("lithia: cannot start the defer thread");
        sender
    });
