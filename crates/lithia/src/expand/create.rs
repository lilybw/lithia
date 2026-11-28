// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Node creation, called by `node!` expansions (spec §4.5, §11.4 step 7).

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::sync::{ArcCell, AtomicBool, AtomicU32, AtomicUsize, Mutex, MutexGuard};
use tokio::runtime::Handle;

use crate::context;
use crate::diag::DiagState;
use crate::expand::capture::{Captured, SharedLease};
use crate::expand::keyed::{self, Inputs, Key, Keyed};
use crate::graph::epoch::Scope;
use crate::graph::ids::{self, Atomic, NodeId, RunId, Stamp, WriterId};
use crate::graph::lineage::Lineage;
use crate::graph::node::{
    Body, BodySlot, BoxFut, Header, KeyedState, Kind, Lifetime, Node, NodeRef, Parents, Sched, SchedGuard,
    TypedNodeRef, WeakNodeRef, phase,
};
use crate::handles::{DerivedNode, NodeHandle, SourceNode};
use crate::options::{Equals, Implicit, Options};
use crate::run::{RunControl, Yielder};
use crate::site::SiteKey;
use crate::{NodeValue, Readable, Site, protocol, testing::progress};

/// Type-erased dependency of a derived node; requires `Readable`.
/// `let deps: [NodeHandle; 1] = [node_ref(&count)];`
#[doc(hidden)]
pub fn node_ref<R: Readable>(handle: &R) -> NodeHandle {
    handle.__lithia_node()
}

/// Fixes the output type of a body's future: `output::<(), _>(async { loop {} })`.
#[doc(hidden)]
pub fn output<T, F: Future<Output = T>>(future: F) -> F {
    future
}

/// Flags a dependency a `move` body is about to take as internal; works in generic code.
/// `move_dep(&count, true);`
#[doc(hidden)]
pub fn move_dep<R: Readable>(handle: &R, read_only: bool) {
    handle.__lithia_move_dep(read_only)
}

fn runtime_for(kind: Kind, site: &Site) -> Option<Handle> {
    // Loom models run every task on their own executor.
    if cfg!(lithia_loom) {
        return None;
    }
    match Handle::try_current() {
        Ok(handle) => Some(handle),
        Err(_) if kind == Kind::Source => None,
        Err(_) => panic!(
            "lithia: a derived node or an async source block needs a tokio runtime, \
             as tokio::spawn does ({site})"
        ),
    }
}

/// Whether a reclaimed node's body may write the same nodes the new captures would.
fn same_targets(header: &Header, captured: &Captured) -> bool {
    let mut wanted: Vec<NodeId> = captured
        .0
        .iter()
        .filter(|(_, writes): &&(NodeRef, bool)| *writes)
        .map(|(node, _): &(NodeRef, bool)| node.header().id)
        .collect();
    wanted.sort_unstable();
    wanted.dedup();
    let mut held: Vec<NodeId> =
        header.targets.iter().filter_map(WeakNodeRef::upgrade).map(|node: NodeRef| node.header().id).collect();
    held.sort_unstable();
    held.dedup();
    wanted == held
}

/// Hands a reclaimed node what the new creation brought: its comparator, a source node its value,
/// a body its captures, then a rerun (spec §10.1, Keyed nodes). Counts the returned handle as an anchor.
fn refresh<K: NodeValue>(
    node: &TypedNodeRef<K>,
    value: K,
    body: BodySlot<K>,
    inputs: Inputs,
    equals: Option<Equals<K>>,
    lease: Option<SharedLease>,
) {
    let header: &Header = &node.header;
    crate::lifetime::support::add_anchors(header, 1, || node.clone());
    let runtime: Option<Handle> = header.runtime.clone();
    if let Some(old) = node.adopt_equals(equals) {
        old.release(runtime.as_ref());
    }
    match header.kind {
        Kind::Source => {
            // A write the node's `equals` may still find equal; a ghost is left alone.
            let _ = node.write(false, move |_: &K| value);
            inputs.release(runtime.as_ref());
            return;
        }
        Kind::Derived => {}
        Kind::AsyncSource => {
            // The running block stops, and the new one runs under the next run id.
            let next: RunId = header.runs.advance(Ordering::SeqCst);
            header.block_run.store(next, Ordering::SeqCst);
        }
    }
    let old: BodySlot<K> = std::mem::replace(&mut *node.body.lock(), body);
    let old_inputs: Option<Inputs> = header.keyed.inputs.lock().replace(inputs);
    let erased: NodeRef = node.clone();
    if header.kind == Kind::AsyncSource {
        protocol::kill_run(&erased);
        // The old block stops: what it moved out counts from now on.
        let old: Option<SharedLease> = std::mem::replace(&mut *header.lifetime.lease.lock(), lease);
        if let Some(old) = old {
            old.end();
        }
    }
    // A run the node owes already, gated until now, runs the new body: no rerun is needed for it.
    let owed: bool = header.kind == Kind::Derived && header.lock_sched().owes_a_run();
    if !owed {
        context::record_refresh(&erased);
    }
    if let Some(old) = old {
        crate::lifetime::drops::drop_later(old, runtime.as_ref());
    }
    if let Some(old) = old_inputs {
        old.release(runtime.as_ref());
    }
    // The unused initial value; only one whose drop can run user code leaves this thread.
    crate::lifetime::drops::drop_owned(value, runtime.as_ref());
}

/// A reclaimed node keeps its lineage, which the creator's next run then finds too.
fn keep_lineage(header: &Header) {
    let creator: Option<NodeRef> = context::creating_run().and_then(|run: Arc<RunControl>| run.node.upgrade());
    if let (Some(creator), Some(lineage), Some(key)) = (creator, &header.lineage, &header.keyed.key) {
        creator.header().lineage_root().keep_keyed(std::ptr::from_ref(header.site).addr(), key.hash, lineage);
    }
}

fn new_node<K: NodeValue>(
    site: &'static Site,
    kind: Kind,
    deps: &[NodeHandle],
    captured: &Captured,
    value: K,
    body: BodySlot<K>,
    mut options: Options<K>,
) -> TypedNodeRef<K> {
    let (key, inputs): (Option<Key>, Option<Inputs>) = match options.keyed.take() {
        None => (None, None),
        Some(mut keyed) => {
            if let Some(reclaimed) = keyed.reclaimed.take() {
                let node: TypedNodeRef<K> = keyed::typed(reclaimed);
                if same_targets(&node.header, captured) {
                    let (key, inputs): (Key, Inputs) = keyed.key(site);
                    refresh(&node, value, body, inputs, options.equals, captured.2.clone());
                    keep_lineage(&node.header);
                    keyed::decided(&node);
                    // The node keeps the key it was created with; this copy may carry user drop glue.
                    crate::lifetime::drops::drop_later(key, node.header.runtime.as_ref());
                    return node;
                }
                // Its body would write other nodes now: a new node takes the key instead.
                let erased: NodeRef = node;
                keyed::unclaim(&erased);
            }
            let (key, inputs): (Key, Inputs) = Keyed::key(keyed, site);
            (Some(key), Some(inputs))
        }
    };
    let runtime: Option<Handle> = runtime_for(kind, site);
    let sync_body: bool = matches!(body, Some(Body::Sync(_)));
    let mut parents: Parents = Vec::with_capacity(deps.len());
    for dep in deps {
        let header: &Header = dep.0.header();
        if !parents.iter().any(|p: &NodeRef| p.header().id == header.id) {
            parents.push(dep.0.clone());
        }
    }
    parents.sort_by_key(|p: &NodeRef| p.header().id);
    let parent_ids: Box<[NodeId]> = parents.iter().map(|p: &NodeRef| p.header().id).collect();
    let run: Option<Arc<RunControl>> = context::creating_run();
    let scope: Arc<Scope> = context::current_scope();
    let creator: Option<NodeId> = run.as_ref().and_then(|r: &Arc<RunControl>| r.node_id);
    let pending: bool = kind != Kind::Source;
    let mut targets: Vec<NodeRef> = Vec::new();
    for (node, writes) in &captured.0 {
        if *writes && !targets.iter().any(|known: &NodeRef| known.header().id == node.header().id) {
            targets.push(node.clone());
        }
    }
    let creator_node: Option<NodeRef> = run.as_ref().and_then(|r: &Arc<RunControl>| r.node.upgrade());
    let id: NodeId = ids::next_node_id();
    // A node created in a run writes under the lineage its creation shares with the same creation
    // in the creator's next run: by its key, or else its place among the site's creations (spec §7.4).
    let lineage: Option<Arc<Lineage>> = match (&run, &creator_node) {
        (Some(run), Some(creator)) => {
            let at: SiteKey = std::ptr::from_ref(site).addr();
            let (from, place): (Arc<Lineage>, u64) =
                run.creation(at, key.is_some(), || creator.header().lineage_root());
            Some(match &key {
                Some(key) => from.keyed(at, key),
                None => from.nth(at, place),
            })
        }
        _ => None,
    };
    let writer: WriterId = lineage.as_ref().map_or(WriterId::node(id), |lineage: &Arc<Lineage>| lineage.id);
    let closed: bool = creator_node.as_ref().is_some_and(|c: &NodeRef| c.header().lock_sched().closed);
    let held: bool = creator_node.is_some() && !closed;
    let top_level: bool = run.is_none();
    let supportable: bool = top_level && !parents.is_empty() && kind != Kind::Source;
    let warn_first_run: bool = kind != Kind::Source && (top_level || (!held && !closed));
    scope.live.fetch_add(1, Ordering::SeqCst);
    let node: TypedNodeRef<K> = Arc::new(Node {
        header: Header {
            id,
            site,
            creator,
            kind,
            is_unit: Node::<K>::is_unit(),
            value_type: std::any::TypeId::of::<K>(),
            shape: options.shape,
            version: Atomic::new(Stamp::NONE),
            phase: AtomicU32::new(if pending { phase::PENDING } else { 0 }),
            failure: ArcCell::new(None),
            runs: Atomic::new(if kind == Kind::AsyncSource { RunId::FIRST } else { RunId::NONE }),
            block_run: Atomic::new(if kind == Kind::AsyncSource { RunId::FIRST } else { RunId::NONE }),
            write: Mutex::new(()),
            sched: Mutex::new(Sched::new(kind != Kind::Source)),
            parents: Mutex::new(Some(parents.clone())),
            parent_ids,
            targets: targets.iter().map(Arc::downgrade).collect(),
            writer,
            lineage,
            runtime,
            sync_body,
            scope,
            changes: Mutex::new(None),
            streamed: AtomicBool::new(false),
            lifetime: Lifetime {
                counted: AtomicBool::new(true),
                anchors: AtomicUsize::new(1),
                captured: AtomicUsize::new(0),
                support: Mutex::new(None),
                let_go: AtomicBool::new(false),
                scoped: AtomicBool::new(false),
                supportable,
                held_by: Mutex::new(None),
                writers: Mutex::new(Vec::new()),
                lease: Mutex::new(if kind == Kind::AsyncSource { captured.2.clone() } else { None }),
            },
            keyed: KeyedState { key, inputs: Mutex::new(inputs), undecided: AtomicBool::new(false) },
            diag: DiagState::new(warn_first_run),
        },
        value: ArcCell::new(Some(Arc::new(value))),
        body: Mutex::new(body),
        equals: options.equals.map(Mutex::new),
    });
    let erased: NodeRef = node.clone();
    // A new child of ghosts is linked as disposal leaves a surviving one; with only ghosts for
    // parents, it is disposed as disposal would have (spec §10.5).
    let orphan: bool = !parents.is_empty() && protocol::link(&erased, &parents);
    drop(parents);
    let keyed: bool = erased.header().keyed.key.is_some();
    if keyed && top_level {
        crate::diag::key_outside_run(site);
    }
    if let Some(creator) = creator_node.as_ref() {
        let mut guard: SchedGuard<'_> = creator.header().lock_sched();
        if !guard.closed {
            *erased.header().lifetime.held_by.lock() = Some(Arc::downgrade(creator));
        }
        let alike: Vec<NodeRef> = guard.note_created(&erased, keyed);
        drop(guard);
        if keyed::is_duplicate(&erased, &alike) {
            crate::diag::duplicate_key(site);
        }
        drop(alike);
        progress::bump();
    }
    drop(creator_node);
    if supportable {
        // Teardown takes support's reference under this lock once the node is a ghost, so a node
        // disposed of while it is created is not held (spec §10.4).
        let mut slot: MutexGuard<'_, Option<NodeRef>> = erased.header().lifetime.support.lock();
        let held: bool = !erased.header().is_ghost();
        if held {
            *slot = Some(erased.clone());
        }
        drop(slot);
        if held {
            crate::diag::support_held(site);
        }
    }
    // Once the node is held, so that its writer edges find it supported.
    if !targets.is_empty() {
        crate::lifetime::support::add_writer(&erased, &targets);
    }
    drop(targets);
    if kind != Kind::Source {
        context::record_creation(&erased);
    }
    if orphan {
        crate::lifetime::dispose::dispose_unchecked(&erased);
    }
    node
}

pin_project_lite::pin_project! {
    /// A one-shot block that ends its lease once dropped: fields drop in order, so the lease ends
    /// after what the block still owns (spec §4.2).
    struct LeasedBlock<F> {
        #[pin]
        block: F,
        lease: EndsLease,
    }
}

impl<F: Future> Future for LeasedBlock<F> {
    type Output = F::Output;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<F::Output> {
        self.project().block.poll(cx)
    }
}

/// Ends a lease when dropped.
struct EndsLease(SharedLease);

impl Drop for EndsLease {
    fn drop(&mut self) {
        self.0.end();
    }
}

/// Creates a source node holding `value` (spec §3.1).
#[doc(hidden)]
pub fn source<K: NodeValue>(
    site: &'static Site,
    value: K,
    options: Options<K>,
    implicit: impl FnOnce(Implicit<K>) -> Option<Equals<K>>,
) -> SourceNode<K> {
    SourceNode {
        node: new_node(site, Kind::Source, &[], &Captured::default(), value, None, options.or_implicit(implicit)),
        internal: crate::handles::HandleCount::new(false),
    }
}

/// Creates a source node fed by an async block, pending with `init` until its first item (spec §8).
#[doc(hidden)]
pub fn source_async<K, F, I>(
    site: &'static Site,
    captured: &Captured,
    init: K,
    block: F,
    options: Options<K>,
    implicit: I,
) -> SourceNode<K>
where
    K: NodeValue,
    F: Future<Output = K> + Send + 'static,
    I: FnOnce(Implicit<K>) -> Option<Equals<K>>,
{
    let body: Body<K> = Body::Once(match captured.2.clone() {
        Some(lease) => Box::pin(LeasedBlock { block, lease: EndsLease(lease) }),
        None => Box::pin(block),
    });
    SourceNode {
        node: new_node(site, Kind::AsyncSource, &[], captured, init, Some(body), options.or_implicit(implicit)),
        internal: crate::handles::HandleCount::new(false),
    }
}

/// Creates a source node fed by an async block that yields (spec §3.6).
#[doc(hidden)]
pub fn source_gen<K, G, F, I>(
    site: &'static Site,
    captured: &Captured,
    init: K,
    block: G,
    options: Options<K>,
    implicit: I,
) -> SourceNode<K>
where
    K: NodeValue,
    G: FnOnce(Yielder<K>) -> F + Send + 'static,
    F: Future<Output = ()> + Send + 'static,
    I: FnOnce(Implicit<K>) -> Option<Equals<K>>,
{
    let lease: Option<SharedLease> = captured.2.clone();
    let start: Box<dyn FnOnce(Yielder<K>) -> BoxFut<()> + Send> = Box::new(move |yielder: Yielder<K>| -> BoxFut<()> {
        match lease {
            Some(lease) => Box::pin(LeasedBlock { block: block(yielder), lease: EndsLease(lease) }),
            None => Box::pin(block(yielder)),
        }
    });
    let body: Body<K> = Body::OnceGen(start);
    SourceNode {
        node: new_node(site, Kind::AsyncSource, &[], captured, init, Some(body), options.or_implicit(implicit)),
        internal: crate::handles::HandleCount::new(false),
    }
}

/// Creates a derived node with a synchronous body (spec §3.1).
#[doc(hidden)]
pub fn derived<K, B, I>(
    site: &'static Site,
    deps: &[NodeHandle],
    captured: &Captured,
    init: K,
    body: B,
    options: Options<K>,
    implicit: I,
) -> DerivedNode<K>
where
    K: NodeValue,
    B: Fn() -> K + Send + Sync + 'static,
    I: FnOnce(Implicit<K>) -> Option<Equals<K>>,
{
    let body: Body<K> = Body::Sync(Arc::new(body));
    DerivedNode {
        node: new_node(site, Kind::Derived, deps, captured, init, Some(body), options.or_implicit(implicit)),
        internal: crate::handles::HandleCount::new(false),
    }
}

/// Creates a derived node with an async body (spec §8).
#[doc(hidden)]
pub fn derived_async<K, B, F, I>(
    site: &'static Site,
    deps: &[NodeHandle],
    captured: &Captured,
    init: K,
    body: B,
    options: Options<K>,
    implicit: I,
) -> DerivedNode<K>
where
    K: NodeValue,
    B: Fn() -> F + Send + Sync + 'static,
    F: Future<Output = K> + Send + 'static,
    I: FnOnce(Implicit<K>) -> Option<Equals<K>>,
{
    let body: Body<K> = Body::Async(Arc::new(move || -> BoxFut<K> { Box::pin(body()) }));
    DerivedNode {
        node: new_node(site, Kind::Derived, deps, captured, init, Some(body), options.or_implicit(implicit)),
        internal: crate::handles::HandleCount::new(false),
    }
}

/// Creates a derived node with a generator body (spec §3.6).
#[doc(hidden)]
pub fn derived_gen<K, B, F, I>(
    site: &'static Site,
    deps: &[NodeHandle],
    captured: &Captured,
    init: K,
    body: B,
    options: Options<K>,
    implicit: I,
) -> DerivedNode<K>
where
    K: NodeValue,
    B: Fn(Yielder<K>) -> F + Send + Sync + 'static,
    F: Future<Output = ()> + Send + 'static,
    I: FnOnce(Implicit<K>) -> Option<Equals<K>>,
{
    let body: Body<K> = Body::Gen(Arc::new(move |yielder: Yielder<K>| -> BoxFut<()> { Box::pin(body(yielder)) }));
    DerivedNode {
        node: new_node(site, Kind::Derived, deps, captured, init, Some(body), options.or_implicit(implicit)),
        internal: crate::handles::HandleCount::new(false),
    }
}
