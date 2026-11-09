//! Free functions of the public API (spec §3.5).

use std::collections::BTreeMap;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::sync::futures::Notified;
use tokio::task::JoinHandle;

use crate::context::{self, Batch, BlockingTarget, Deferred, Frame, FrameGuard, FrameKind};
use crate::epoch::{Epoch, Scope};
use crate::handles::NodeHandle;
use crate::ids::{self, NodeId};
use crate::node::{Header, NodeRef, Sched, Served, WeakNodeRef};
use crate::protocol::ServedEpochs;
use crate::run::{self, RunShared};
use crate::sync::MutexGuard;
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
        let guard: MutexGuard<'_, Sched> = node.header().sched.lock();
        match &guard.run {
            Some(slot) if slot.id == run.id() && !run.is_cancelled() => {
                Some(slot.served.iter().map(|s: &Served| (s.epoch.clone(), s.writers.clone())).collect())
            }
            _ => None,
        }
    };
    served.map(|served: ServedEpochs| (node, served))
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

/// Delivers the pending batch, then waits until every run in the caller's scope has ended or suspended.
/// `lithia::flush().await;` never waits for async results; tests use `lithia::test::settled`.
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

/// Raises the node's version and reruns it after any run in flight; its children rerun even on an equal value.
/// `lithia::reflow(&doubled);` from the node's own body reruns only its children.
pub fn reflow<N: Reflow>(node: &N) {
    let handle: NodeHandle = node.__lithia_reflow_node();
    if try_reflow(node).is_err() {
        panic!("lithia: reflow of a disposed node ({})", handle.0.header().site);
    }
}

/// Like `reflow`, but returns `Err(Ghosted)` instead of panicking on a ghost.
/// `assert_eq!(lithia::try_reflow(&disposed), Err(lithia::Ghosted));`
pub fn try_reflow<N: Reflow>(node: &N) -> Result<(), Ghosted> {
    context::assert_not_in_closure("lithia::reflow");
    let handle: NodeHandle = node.__lithia_reflow_node();
    let header: &Header = handle.0.header();
    let stamp: u64 = {
        let _write: MutexGuard<'_, ()> = header.write.lock();
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
/// ⚠ Use-after-free hazard: remaining handles become ghosts. `lithia::dispose(&cache);`
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

/// Runs blocking work on tokio's blocking pool in the caller's scope; from a body, its writes count as the run's.
/// `lithia::spawn_blocking(move || count.set(heavy())).await?;`
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
            let _entered: FrameGuard<'_> = context::enter(&frame);
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

    /// Delivers the pending batch, then waits until every run started in the node's subtree has ended.
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
            let notified: Notified<'_> = waiter.notify().notified();
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

    /// Number of live nodes created in the caller's scope, once pending payload drops have run.
    /// Exact once the scope has settled. `assert_eq!(lithia::test::live_nodes().await, before);`
    pub async fn live_nodes() -> usize {
        let scope: Arc<Scope> = context::current_scope();
        let mut quiet: u32 = 0;
        for _ in 0..10_000 {
            if crate::drops::queued() == 0 {
                quiet += 1;
                if quiet >= 3 {
                    break;
                }
            } else {
                quiet = 0;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        scope.live.load(Ordering::SeqCst)
    }

    /// Whether `root`, its descendants and the nodes their runs created have no work left,
    /// checked parents first so an idle parent has already delivered to its children.
    fn subtree_quiet(root: &NodeRef) -> bool {
        let mut closure: BTreeMap<NodeId, NodeRef> = BTreeMap::new();
        let mut stack: Vec<NodeRef> = vec![root.clone()];
        while let Some(node) = stack.pop() {
            let header: &Header = node.header();
            if closure.contains_key(&header.id) {
                continue;
            }
            let created: Vec<NodeRef> = {
                let guard: MutexGuard<'_, Sched> = header.sched.lock();
                guard.created.iter().filter_map(|weak: &WeakNodeRef| weak.upgrade()).collect()
            };
            stack.extend(header.subscribers());
            stack.extend(created);
            closure.insert(header.id, node);
        }
        closure.values().all(|node: &NodeRef| {
            let guard: MutexGuard<'_, Sched> = node.header().sched.lock();
            !(guard.busy() || guard.unseeded || !guard.entries.is_empty())
        })
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
