//! Free functions of the public API (spec §3.5).

use std::collections::HashSet;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::context::{self, Batch, BlockingTarget, Deferred, Frame, FrameKind};
use crate::epoch::{Epoch, Scope};
use crate::handles::NodeHandle;
use crate::ids::{self, NodeId};
use crate::node::{NodeRef, Served};
use crate::protocol::ServedEpochs;
use crate::run::{self, RunShared};
use crate::{Dispose, Ghosted, Readable, Reflow, Status, dispose as disposal, protocol, settle_wait};

/// What a flush waits for once its batch is delivered.
enum Wait {
    Scope(Arc<Scope>),
    Epochs(Vec<Arc<Epoch>>),
}

/// Takes the current frame's pending batch, or an empty one outside lithia tasks.
fn take_pending() -> (Batch, Option<FrameTarget>) {
    context::with_frame(|frame: Option<&Frame>| match frame {
        None => (Batch::default(), None),
        Some(frame) => {
            let target: FrameTarget = match &frame.kind {
                FrameKind::Task(scope) => FrameTarget::Scope(scope.clone()),
                FrameKind::Run(run) => FrameTarget::Run(run.clone()),
                FrameKind::Blocking(BlockingTarget::Run(run)) => FrameTarget::Run(run.clone()),
                FrameKind::Blocking(BlockingTarget::Scope(scope)) => FrameTarget::Scope(scope.clone()),
            };
            (frame.take_batch(), Some(target))
        }
    })
}

enum FrameTarget {
    Scope(Arc<Scope>),
    Run(Arc<RunShared>),
}

/// The epochs a run serves, with their writer sets, if `run` is still in flight.
fn served_by(run: &Arc<RunShared>) -> Option<(NodeRef, ServedEpochs)> {
    let node: NodeRef = run.node.upgrade()?;
    let served: Option<ServedEpochs> = {
        let guard = node.header().sched.lock();
        match &guard.run {
            Some(slot) if slot.id == run.id() && !run.is_cancelled() => {
                Some(slot.served.iter().map(|s: &Served| (s.epoch.clone(), s.writers.clone())).collect())
            }
            _ => None,
        }
    };
    served.map(|served| (node, served))
}

/// Delivers a run's batch as child epochs while it is in flight, else as an outside batch.
fn deliver_for_run(run: &Arc<RunShared>, batch: Batch) -> Vec<Arc<Epoch>> {
    match served_by(run) {
        Some((node, served)) => {
            let epochs: Vec<Arc<Epoch>> = protocol::deliver_children(&node, &served, batch, 0);
            drop(node);
            protocol::close_all(epochs.clone());
            epochs
        }
        None => {
            protocol::deliver_outside(batch, &run.scope());
            Vec::new()
        }
    }
}

/// Delivers the pending batch, then waits until every run in the caller's scope has finished or suspended (11.3).
/// It never waits for async results; tests use `lithia::test::settled`.
/// `lithia::flush().await;`
pub async fn flush() {
    let (batch, target): (Batch, Option<FrameTarget>) = take_pending();
    let wait: Wait = match target {
        None => Wait::Scope(context::detached_scope()),
        Some(FrameTarget::Scope(scope)) => {
            protocol::deliver_outside(batch, &scope);
            Wait::Scope(scope)
        }
        Some(FrameTarget::Run(run)) => Wait::Epochs(deliver_for_run(&run, batch)),
    };
    match wait {
        Wait::Scope(scope) => scope.wait_settled().await,
        Wait::Epochs(epochs) => {
            for epoch in epochs {
                epoch.wait_subtree().await;
            }
        }
    }
}

/// Raises the node's version and reruns its body after any run in flight, so its children rerun even on an equal value.
/// From the node's own body only its children rerun (7.4).
/// `lithia::reflow(&doubled);`
pub fn reflow<N: Reflow>(node: &N) {
    let handle: NodeHandle = node.__lithia_reflow_node();
    if try_reflow(node).is_err() {
        panic!("lithia: reflow of a disposed node ({})", handle.0.header().site);
    }
}

/// Like `reflow`, but returns `Err(Ghosted)` instead of panicking on a ghost.
pub fn try_reflow<N: Reflow>(node: &N) -> Result<(), Ghosted> {
    context::assert_not_in_closure("lithia::reflow");
    let handle: NodeHandle = node.__lithia_reflow_node();
    let header = handle.0.header();
    let stamp: u64 = {
        let _write = header.write.lock();
        if header.is_ghost() {
            return Err(Ghosted);
        }
        let stamp: u64 = ids::stamp();
        header.version.store(stamp, Ordering::SeqCst);
        stamp
    };
    context::record_reflow(&handle.0, stamp);
    Ok(())
}

/// Frees the node and every descendant with no live parent outside the freed branch, regardless of handles.
/// ⚠ Use-after-free hazard: remaining handles become ghosts governed by each node's `ghost` option.
pub fn dispose<N: Dispose>(node: &N) {
    let handle: NodeHandle = node.__lithia_dispose_node();
    disposal::dispose(&handle.0);
}

/// Runs `f` once the current batch's epoch has settled; a thin helper over the epoch tree (7.1).
/// `lithia::defer(move || done.set(true));`
pub fn defer(f: impl FnOnce() + Send + 'static) {
    let scope: Arc<Scope> = context::current_scope();
    scope.inc();
    let runtime: Option<Handle> = Handle::try_current().ok();
    context::record_defer(Deferred { f: Box::new(f), scope, runtime });
}

/// Current condition of the node.
/// `let s: Status = lithia::status(&user);`
pub fn status<N: Readable>(node: &N) -> Status {
    node.__lithia_node().status()
}

/// Comparator for the `equals` option, using `PartialEq`.
/// `let n: SourceNode<i32> = effect!(0, equals = lithia::eq);`
pub fn eq<K: PartialEq>(old: &K, new: &K) -> bool {
    old == new
}

/// Spawns a task that roots its own scope and whose polls form batches; it starts with an empty writer set.
/// `lithia::spawn(async move { count.set(1); });`
pub fn spawn<F: Future<Output = ()> + Send + 'static>(fut: F) -> JoinHandle<()> {
    tokio::spawn(run::scoped(fut))
}

/// Runs blocking work on tokio's blocking pool in the caller's scope; from a body, its writes arrive as one of the
/// run's deliveries while the run is in flight (7.1).
/// `lithia::spawn_blocking(move || count.set(heavy()));`
pub fn spawn_blocking<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> JoinHandle<R> {
    let target: BlockingTarget = context::with_frame(|frame: Option<&Frame>| match frame {
        Some(frame) => match &frame.kind {
            FrameKind::Run(run) => BlockingTarget::Run(run.clone()),
            FrameKind::Blocking(target) => target.clone(),
            FrameKind::Task(scope) => BlockingTarget::Scope(scope.clone()),
        },
        None => BlockingTarget::Scope(context::detached_scope()),
    });
    tokio::task::spawn_blocking(move || {
        let frame: Frame = Frame::new(FrameKind::Blocking(target.clone()));
        let result: std::thread::Result<R> = {
            let _entered = context::enter(&frame);
            catch_unwind(AssertUnwindSafe(f))
        };
        let batch: Batch = frame.take_batch();
        drop(frame);
        match &target {
            BlockingTarget::Run(run) => {
                deliver_for_run(run, batch);
            }
            BlockingTarget::Scope(scope) => protocol::deliver_outside(batch, scope),
        }
        match result {
            Ok(value) => value,
            Err(panic) => resume_unwind(panic),
        }
    })
}

/// Test utilities; unstable, and exempt from stability guarantees.
pub mod test_utils {
    use super::*;

    /// Delivers the pending batch, then waits until every run started in the node's subtree has ended (11.3).
    /// Unstable: may change in any release. `lithia::test::settled(&user).await;`
    pub async fn settled<N: Readable>(node: &N) {
        let (batch, target): (Batch, Option<FrameTarget>) = take_pending();
        match target {
            None => {}
            Some(FrameTarget::Scope(scope)) => protocol::deliver_outside(batch, &scope),
            Some(FrameTarget::Run(run)) => {
                deliver_for_run(&run, batch);
            }
        }
        let root: NodeRef = node.__lithia_node().0;
        let waiter: settle_wait::Waiter = settle_wait::Waiter::new();
        loop {
            let notified = waiter.notify().notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let before: u64 = waiter.seq();
            let quiet: bool = subtree_quiet(&root);
            let after: u64 = waiter.seq();
            if quiet && before == after {
                return;
            }
            if before != after {
                continue;
            }
            tokio::select! {
                _ = &mut notified => {}
                _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
        }
    }

    /// Number of live nodes created in the caller's scope (11.3); lets tests catch leaked value cycles.
    pub fn live_nodes() -> usize {
        context::current_scope().live.load(Ordering::SeqCst)
    }

    /// Whether `root`, its descendants and every node their runs created have no work left.
    fn subtree_quiet(root: &NodeRef) -> bool {
        let mut seen: HashSet<NodeId> = HashSet::new();
        let mut stack: Vec<NodeRef> = vec![root.clone()];
        while let Some(node) = stack.pop() {
            let header = node.header();
            if !seen.insert(header.id) {
                continue;
            }
            let (busy, created): (bool, Vec<NodeRef>) = {
                let guard = header.sched.lock();
                let busy: bool = guard.busy() || guard.unseeded || !guard.entries.is_empty();
                let created: Vec<NodeRef> = guard.created.iter().filter_map(|weak| weak.upgrade()).collect();
                (busy, created)
            };
            if busy {
                return false;
            }
            stack.extend(header.subscribers());
            stack.extend(created);
        }
        true
    }
}

/// Reads the test timeout the workspace harness sets, if any.
fn test_timeout() -> Option<Duration> {
    std::env::var("LITHIA_TEST_TIMEOUT_MS")
        .ok()
        .and_then(|raw: String| raw.trim().parse::<u64>().ok())
        .filter(|ms: &u64| *ms > 0)
        .map(Duration::from_millis)
}

/// Runs an entry future as a lithia task on a fresh runtime; used by `#[lithia::main]`.
#[doc(hidden)]
pub fn block_on_main<F: Future>(mut builder: tokio::runtime::Builder, future: F) -> F::Output {
    let runtime: tokio::runtime::Runtime =
        builder.enable_all().build().expect("lithia: cannot build the tokio runtime");
    runtime.block_on(run::scoped(future))
}

/// Runs a test body as a lithia task on a fresh runtime, under the harness timeout if set; used by `#[lithia::test]`.
#[doc(hidden)]
pub fn block_on_test<F: Future<Output = ()>>(mut builder: tokio::runtime::Builder, name: &str, future: F) {
    let runtime: tokio::runtime::Runtime =
        builder.enable_all().build().expect("lithia: cannot build the tokio runtime");
    let timeout: Option<Duration> = test_timeout();
    runtime.block_on(async move {
        match timeout {
            Some(limit) => {
                if tokio::time::timeout(limit, run::scoped(future)).await.is_err() {
                    panic!("lithia: test `{name}` timed out after {limit:?}");
                }
            }
            None => run::scoped(future).await,
        }
    });
}
