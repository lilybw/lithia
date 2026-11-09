//! Node creation, called by `effect!` expansions (spec §4.5, §11.4 step 7).

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::sync::{ArcCell, AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Mutex, MutexGuard};
use tokio::runtime::Handle;

use crate::capture::Captured;
use crate::context;
use crate::epoch::Scope;
use crate::handles::{DerivedNode, NodeHandle, SourceNode};
use crate::ids::{self, NodeId};
use crate::node::{Body, BoxFut, Header, Kind, Node, NodeRef, RateWindow, Sched, WeakNodeRef, phase};
use crate::options::Options;
use crate::run::{RunShared, Yielder};
use crate::{NodeValue, Readable, Site, protocol, settle_wait};

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

fn new_node<K: NodeValue>(
    site: &'static Site,
    kind: Kind,
    deps: &[NodeHandle],
    captured: &Captured,
    value: K,
    body: Option<Body<K>>,
    options: Options<K>,
) -> Arc<Node<K>> {
    options.validate(site);
    let runtime: Option<Handle> = runtime_for(kind, site);
    let mut parents: Vec<NodeRef> = Vec::with_capacity(deps.len());
    for dep in deps {
        let header: &Header = dep.0.header();
        if header.is_ghost() {
            panic!("lithia: cannot create a node that depends on a disposed node ({site}; parent {})", header.site);
        }
        if !parents.iter().any(|p: &NodeRef| p.header().id == header.id) {
            parents.push(dep.0.clone());
        }
    }
    parents.sort_by_key(|p: &NodeRef| p.header().id);
    let parent_ids: Box<[NodeId]> = parents.iter().map(|p: &NodeRef| p.header().id).collect();
    let run: Option<Arc<RunShared>> = context::creating_run();
    let scope: Arc<Scope> = context::current_scope();
    let creator: Option<NodeId> = run.as_ref().map(|r: &Arc<RunShared>| r.node_id);
    let pending: bool = kind != Kind::Source;
    let mut targets: Vec<NodeRef> = Vec::new();
    for (node, writes) in &captured.0 {
        if *writes && !targets.iter().any(|known: &NodeRef| known.header().id == node.header().id) {
            targets.push(node.clone());
        }
    }
    let creator_node: Option<NodeRef> = run.as_ref().and_then(|r: &Arc<RunShared>| r.node.upgrade());
    let closed: bool = creator_node.as_ref().is_some_and(|c: &NodeRef| c.header().sched.lock().closed);
    let held: bool = creator_node.is_some() && !closed;
    let top_level: bool = run.is_none();
    let warn_first_run: bool = kind != Kind::Source && (top_level || (!held && !closed));
    let (sender, _): (tokio::sync::watch::Sender<u64>, tokio::sync::watch::Receiver<u64>) =
        tokio::sync::watch::channel(0u64);
    scope.live.fetch_add(1, Ordering::SeqCst);
    let node: Arc<Node<K>> = Arc::new(Node {
        header: Header {
            id: ids::next_node_id(),
            site,
            creator,
            kind,
            is_unit: Node::<K>::is_unit(),
            version: AtomicU64::new(0),
            phase: AtomicU32::new(if pending { phase::PENDING } else { 0 }),
            failure: ArcCell::new(None),
            runs: AtomicU64::new(if kind == Kind::AsyncSource { 1 } else { 0 }),
            write: Mutex::new(()),
            sched: Mutex::new(Sched::new(kind != Kind::Source)),
            parents: Mutex::new(Some(parents.clone())),
            parent_ids,
            fragility: options.fragility,
            ghost: options.ghost,
            flags: options.flags,
            runtime,
            scope,
            counted: AtomicBool::new(true),
            changes: Mutex::new(Some(sender)),
            rate: Mutex::new(RateWindow { start_ms: 0, count: 0, warned: false }),
            anchors: AtomicUsize::new(1),
            captured: AtomicUsize::new(0),
            support: Mutex::new(None),
            scoped: AtomicBool::new(false),
            held_by: Mutex::new(None),
            targets: targets.iter().map(Arc::downgrade).collect(),
            writers: Mutex::new(Vec::new()),
            warn_first_run,
            under_way: AtomicBool::new(false),
        },
        value: ArcCell::new(Some(Arc::new(value))),
        body: Mutex::new(body),
        equals: options.equals,
    });
    let erased: NodeRef = node.clone();
    if !parents.is_empty() {
        protocol::link(&erased, &parents);
    }
    let has_parents: bool = !parents.is_empty();
    drop(parents);
    for target in &targets {
        target.header().writers.lock().push(Arc::downgrade(&erased));
    }
    drop(targets);
    if let Some(creator) = creator_node.as_ref() {
        let mut guard: MutexGuard<'_, Sched> = creator.header().sched.lock();
        guard.created.retain(|weak: &WeakNodeRef| weak.strong_count() > 0);
        guard.created.push(Arc::downgrade(&erased));
        if !guard.closed {
            *erased.header().held_by.lock() = Some(Arc::downgrade(creator));
            guard.generation.push(erased.clone());
        }
        drop(guard);
        settle_wait::bump();
    }
    drop(creator_node);
    if top_level && has_parents && kind != Kind::Source {
        *erased.header().support.lock() = Some(erased.clone());
        crate::diag::support_held(site);
    }
    if kind != Kind::Source {
        context::record_creation(&erased);
    }
    node
}

/// Creates a source node holding `value` (spec §3.1).
#[doc(hidden)]
pub fn source<K: NodeValue>(site: &'static Site, value: K, options: Options<K>) -> SourceNode<K> {
    SourceNode {
        node: new_node(site, Kind::Source, &[], &Captured::default(), value, None, options),
        internal: std::sync::atomic::AtomicBool::new(false),
    }
}

/// Creates a source node fed by an async block, pending with `init` until its first item (spec §8).
#[doc(hidden)]
pub fn source_async<K, F>(
    site: &'static Site,
    captured: &Captured,
    init: K,
    block: F,
    options: Options<K>,
) -> SourceNode<K>
where
    K: NodeValue,
    F: Future<Output = K> + Send + 'static,
{
    let body: Body<K> = Body::Once(Box::pin(block));
    SourceNode {
        node: new_node(site, Kind::AsyncSource, &[], captured, init, Some(body), options),
        internal: std::sync::atomic::AtomicBool::new(false),
    }
}

/// Creates a source node fed by an async block that yields (spec §3.6).
#[doc(hidden)]
pub fn source_gen<K, G, F>(
    site: &'static Site,
    captured: &Captured,
    init: K,
    block: G,
    options: Options<K>,
) -> SourceNode<K>
where
    K: NodeValue,
    G: FnOnce(Yielder<K>) -> F + Send + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let start: Box<dyn FnOnce(Yielder<K>) -> BoxFut<()> + Send> =
        Box::new(move |yielder: Yielder<K>| -> BoxFut<()> { Box::pin(block(yielder)) });
    let body: Body<K> = Body::OnceGen(start);
    SourceNode {
        node: new_node(site, Kind::AsyncSource, &[], captured, init, Some(body), options),
        internal: std::sync::atomic::AtomicBool::new(false),
    }
}

/// Creates a derived node with a synchronous body (spec §3.1).
#[doc(hidden)]
pub fn derived<K, B>(
    site: &'static Site,
    deps: &[NodeHandle],
    captured: &Captured,
    init: K,
    body: B,
    options: Options<K>,
) -> DerivedNode<K>
where
    K: NodeValue,
    B: Fn() -> K + Send + Sync + 'static,
{
    let body: Body<K> = Body::Sync(Arc::new(body));
    DerivedNode {
        node: new_node(site, Kind::Derived, deps, captured, init, Some(body), options),
        internal: std::sync::atomic::AtomicBool::new(false),
    }
}

/// Creates a derived node with an async body (spec §8).
#[doc(hidden)]
pub fn derived_async<K, B, F>(
    site: &'static Site,
    deps: &[NodeHandle],
    captured: &Captured,
    init: K,
    body: B,
    options: Options<K>,
) -> DerivedNode<K>
where
    K: NodeValue,
    B: Fn() -> F + Send + Sync + 'static,
    F: Future<Output = K> + Send + 'static,
{
    let body: Body<K> = Body::Async(Arc::new(move || -> BoxFut<K> { Box::pin(body()) }));
    DerivedNode {
        node: new_node(site, Kind::Derived, deps, captured, init, Some(body), options),
        internal: std::sync::atomic::AtomicBool::new(false),
    }
}

/// Creates a derived node with a generator body (spec §3.6).
#[doc(hidden)]
pub fn derived_gen<K, B, F>(
    site: &'static Site,
    deps: &[NodeHandle],
    captured: &Captured,
    init: K,
    body: B,
    options: Options<K>,
) -> DerivedNode<K>
where
    K: NodeValue,
    B: Fn(Yielder<K>) -> F + Send + Sync + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let body: Body<K> = Body::Gen(Arc::new(move |yielder: Yielder<K>| -> BoxFut<()> { Box::pin(body(yielder)) }));
    DerivedNode {
        node: new_node(site, Kind::Derived, deps, captured, init, Some(body), options),
        internal: std::sync::atomic::AtomicBool::new(false),
    }
}
