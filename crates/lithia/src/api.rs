// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Free functions of the public API (spec §3.5).

use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::runtime::Handle;

use crate::context::{self, Batch, Deferred, Frame, Target};
use crate::graph::epoch::{Epochs, Scope, ScopeToken};
use crate::graph::ids::{self, Stamp};
use crate::graph::node::Header;
use crate::handles::NodeHandle;
use crate::run::{self};
use crate::sync::MutexGuard;
use crate::{Dispose, Ghosted, JoinHandle, Readable, Reflow, Status, lifetime::dispose as disposal, protocol};

/// Delivers the pending batch, then waits until every run in the caller's scope has ended or suspended.
/// `lithia::flush().await;` never waits for async results; tests use `lithia::test::settled`.
pub async fn flush() {
    let pending: Option<(Batch, Target)> = protocol::take_pending();
    let (scope, epochs): (Option<Arc<Scope>>, Epochs) = match pending {
        None => (Some(context::detached_scope()), Epochs::new()),
        Some((batch, target)) => {
            let epochs: Epochs = protocol::deliver(&target, batch);
            match target {
                Target::Scope(scope) => (Some(scope), epochs),
                Target::Run(_) => (None, epochs),
            }
        }
    };
    if let Some(scope) = scope {
        scope.wait_settled().await;
    }
    for epoch in epochs {
        epoch.wait_subtree().await;
    }
}

/// Raises the node's version and reruns it after any run in flight; its children rerun even on an equal value.
/// `lithia::reflow(&doubled);` from the node's own body reruns only its children.
#[track_caller]
pub fn reflow<N: Reflow>(node: &N) {
    let handle: NodeHandle = node.__lithia_reflow_node();
    if try_reflow(node).is_err() {
        panic!("lithia: reflow of a disposed node ({})", handle.0.header().site);
    }
}

/// Like `reflow`, but returns `Err(Ghosted)` instead of panicking on a ghost.
/// `assert_eq!(lithia::try_reflow(&disposed), Err(lithia::Ghosted));`
#[track_caller]
pub fn try_reflow<N: Reflow>(node: &N) -> Result<(), Ghosted> {
    context::assert_not_in_closure("lithia::reflow");
    let handle: NodeHandle = node.__lithia_reflow_node();
    let header: &Header = handle.0.header();
    if context::suppressed(header.id) {
        // A cut writer's rerun: it reflowed this node already in the epoch it was cut in.
        return if header.is_ghost() { Err(Ghosted) } else { Ok(()) };
    }
    let stamp: Stamp = {
        let _write: MutexGuard<'_, ()> = header.write.lock();
        if header.is_ghost() {
            return Err(Ghosted);
        }
        let stamp: Stamp = ids::stamp();
        debug_assert!(stamp > header.version.load(Ordering::Relaxed), "lithia: a node's version went back");
        header.version.store(stamp, Ordering::Release);
        stamp
    };
    context::record_reflow(&handle.0, stamp);
    Ok(())
}

/// Frees the node and every descendant with no live parent outside the freed branch, regardless of handles.
/// ⚠ Use-after-free hazard: remaining handles become ghosts. `lithia::dispose(&cache);`
#[track_caller]
pub fn dispose<N: Dispose>(node: &N) {
    let handle: NodeHandle = node.__lithia_dispose_node();
    disposal::dispose(&handle.0);
}

/// Runs `f` once the current batch's epoch has settled; a thin helper over the epoch tree (7.1).
/// `lithia::defer(move || done.set(true));`
pub fn defer(f: impl FnOnce() + Send + 'static) {
    let token: ScopeToken = ScopeToken::take(context::current_scope());
    let runtime: Option<Handle> = Handle::try_current().ok();
    context::record_defer(Deferred { f: Box::new(f), token, runtime });
}

/// Current condition of the node.
/// `let s: Status = lithia::status(&user);`
pub fn status<N: Readable>(node: &N) -> Status {
    node.__lithia_node().status()
}

/// Comparator for the `equals` option, using `PartialEq`.
/// `let n: SourceNode<i32> = node!(0, equals = lithia::eq);`
pub fn eq<K: PartialEq>(old: &K, new: &K) -> bool {
    old == new
}

/// Spawns a task that roots its own scope and whose polls form batches; it starts with an empty writer set.
/// `lithia::spawn(async move { count.set(1); });`
pub fn spawn<F: Future<Output = ()> + Send + 'static>(fut: F) -> JoinHandle<()> {
    JoinHandle::new(tokio::spawn(run::scoped(fut)))
}

/// Runs blocking work on the runtime's blocking pool in the caller's scope; from a body, its writes count as the run's.
/// `lithia::spawn_blocking(move || count.set(heavy())).await?;`
pub fn spawn_blocking<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> JoinHandle<R> {
    let target: Target = context::with_frame(|frame: Option<&Frame>| {
        frame.map_or_else(|| Target::Scope(context::detached_scope()), |frame: &Frame| frame.target.clone())
    });
    JoinHandle::new(tokio::task::spawn_blocking(move || {
        let frame: Frame = Frame::new(target.clone());
        let result: std::thread::Result<R> = context::entered(&frame, || catch_unwind(AssertUnwindSafe(f)));
        let batch: Batch = frame.take_batch();
        drop(frame);
        protocol::deliver(&target, batch);
        match result {
            Ok(value) => value,
            Err(panic) => resume_unwind(panic),
        }
    }))
}
