//! Keyed creation: a run reclaims the node its creator's previous run created with the same key (spec §10.1).

use std::any::{Any, TypeId};
use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use crate::context;
use crate::handles::{DerivedNode, NodeHandle, SourceNode};
use crate::ids::NodeId;
use crate::node::{Header, Kind, Node, NodeRef};
use crate::run::RunShared;
use crate::sync::MutexGuard;
use crate::{NodeValue, Site};

/// A value `effect!`'s `key` option accepts.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be a node key",
    label = "used as `key` here",
    note = "a key must be `Hash + Eq + Send + Sync + 'static`, like a `HashMap` key shared across threads"
)]
pub trait KeyValue: Hash + Eq + Send + Sync + 'static {}

#[diagnostic::do_not_recommend]
impl<T: Hash + Eq + Send + Sync + 'static> KeyValue for T {}

/// A key with its type erased.
trait DynKey: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn same(&self, other: &dyn DynKey) -> bool;
    fn hash_at(&self, site: &'static Site) -> u64;
}

impl<T: KeyValue> DynKey for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn same(&self, other: &dyn DynKey) -> bool {
        other.as_any().downcast_ref::<T>() == Some(self)
    }

    fn hash_at(&self, site: &'static Site) -> u64 {
        key_hash(site, self)
    }
}

/// Hashes a key together with its site and type, so keys of different sites never meet.
fn key_hash<T: KeyValue>(site: &'static Site, value: &T) -> u64 {
    let mut hasher: DefaultHasher = DefaultHasher::new();
    std::ptr::from_ref(site).addr().hash(&mut hasher);
    TypeId::of::<T>().hash(&mut hasher);
    value.hash(&mut hasher);
    hasher.finish()
}

/// A node's key, kept in its header.
pub(crate) struct Key {
    pub(crate) hash: u64,
    value: Box<dyn DynKey>,
}

impl Key {
    fn matches(&self, probe: &KeyRef<'_>) -> bool {
        self.hash == probe.hash && self.value.same(probe.value)
    }
}

/// A key value, checked against `KeyValue` once, where the key is written.
/// `let key: NodeKey<u64> = NodeKey::new(row.id);`
#[doc(hidden)]
pub struct NodeKey<T> {
    value: T,
    erase: fn(&T) -> &dyn DynKey,
    boxed: fn(T) -> Box<dyn DynKey>,
}

impl<T> NodeKey<T> {
    /// Wraps a key value.
    pub fn new(value: T) -> NodeKey<T>
    where
        T: KeyValue,
    {
        NodeKey { value, erase: |value: &T| value, boxed: |value: T| Box::new(value) }
    }

    fn into_box(self) -> Box<dyn DynKey> {
        (self.boxed)(self.value)
    }
}

/// A key looked up before anything is created; borrows the key value.
/// `KeyRef::new(&__LITHIA_SITE, &__lithia_key)`
#[doc(hidden)]
pub struct KeyRef<'a> {
    site: &'static Site,
    hash: u64,
    value: &'a dyn DynKey,
}

impl<'a> KeyRef<'a> {
    /// Hashes the key with its site.
    pub fn new<T>(site: &'static Site, key: &'a NodeKey<T>) -> KeyRef<'a> {
        let value: &'a dyn DynKey = (key.erase)(&key.value);
        KeyRef { site, hash: value.hash_at(site), value }
    }
}

/// One captured value, compared against what a reused node stored (spec §4.7).
#[doc(hidden)]
pub trait Probe {
    /// Whether `stored` holds an equal input.
    fn matches(&self, stored: &(dyn Any + Send + Sync)) -> bool;
    /// What to compare later runs against; `None` when the value cannot be compared.
    fn keep(&self) -> Option<Box<dyn Any + Send + Sync>>;
}

/// A captured value borrowed for a probe. `(&&&&Probed(&row)).__lithia_probe()`
#[doc(hidden)]
pub struct Probed<'a, T: ?Sized>(pub &'a T);

/// A handle compares by its node.
#[doc(hidden)]
pub struct HandleProbe(NodeId);

/// A value compares with `PartialEq`.
#[doc(hidden)]
pub struct EqProbe<'a, T>(&'a T);

/// An `Arc` without `PartialEq` compares by pointer.
#[doc(hidden)]
pub struct ArcProbe<'a, T: ?Sized>(&'a Arc<T>);

/// A value with no `PartialEq` the macro can see never compares equal.
#[doc(hidden)]
pub struct OpaqueProbe;

impl Probe for HandleProbe {
    fn matches(&self, stored: &(dyn Any + Send + Sync)) -> bool {
        stored.downcast_ref::<NodeId>() == Some(&self.0)
    }

    fn keep(&self) -> Option<Box<dyn Any + Send + Sync>> {
        Some(Box::new(self.0))
    }
}

impl<T: PartialEq + Clone + Send + Sync + 'static> Probe for EqProbe<'_, T> {
    fn matches(&self, stored: &(dyn Any + Send + Sync)) -> bool {
        stored.downcast_ref::<T>() == Some(self.0)
    }

    fn keep(&self) -> Option<Box<dyn Any + Send + Sync>> {
        Some(Box::new(self.0.clone()))
    }
}

impl<T: ?Sized + Send + Sync + 'static> Probe for ArcProbe<'_, T> {
    fn matches(&self, stored: &(dyn Any + Send + Sync)) -> bool {
        stored.downcast_ref::<Arc<T>>().is_some_and(|stored: &Arc<T>| Arc::ptr_eq(stored, self.0))
    }

    fn keep(&self) -> Option<Box<dyn Any + Send + Sync>> {
        Some(Box::new(self.0.clone()))
    }
}

impl Probe for OpaqueProbe {
    fn matches(&self, _stored: &(dyn Any + Send + Sync)) -> bool {
        false
    }

    fn keep(&self) -> Option<Box<dyn Any + Send + Sync>> {
        None
    }
}

/// Implemented by every handle type; a probe compares handles by node.
#[doc(hidden)]
pub trait HandleId {
    fn __lithia_node_id(&self) -> NodeId;
}

/// First choice: a handle.
#[doc(hidden)]
pub trait ProbeViaHandle {
    fn __lithia_probe(&self) -> HandleProbe;
}

impl<T: HandleId> ProbeViaHandle for &&&Probed<'_, T> {
    fn __lithia_probe(&self) -> HandleProbe {
        HandleProbe(self.0.__lithia_node_id())
    }
}

/// Second choice: a value with `PartialEq`.
#[doc(hidden)]
pub trait ProbeViaEq<'a, T> {
    fn __lithia_probe(&self) -> EqProbe<'a, T>;
}

impl<'a, T: PartialEq + Clone + Send + Sync + 'static> ProbeViaEq<'a, T> for &&Probed<'a, T> {
    fn __lithia_probe(&self) -> EqProbe<'a, T> {
        EqProbe(self.0)
    }
}

/// Third choice: an `Arc`, by pointer.
#[doc(hidden)]
pub trait ProbeViaArc<'a, T: ?Sized> {
    fn __lithia_probe(&self) -> ArcProbe<'a, T>;
}

impl<'a, T: ?Sized + Send + Sync + 'static> ProbeViaArc<'a, T> for &Probed<'a, Arc<T>> {
    fn __lithia_probe(&self) -> ArcProbe<'a, T> {
        ArcProbe(self.0)
    }
}

/// Last choice: anything else.
#[doc(hidden)]
pub trait ProbeViaOpaque {
    fn __lithia_probe(&self) -> OpaqueProbe;
}

impl<T: ?Sized> ProbeViaOpaque for Probed<'_, T> {
    fn __lithia_probe(&self) -> OpaqueProbe {
        OpaqueProbe
    }
}

/// The inputs a keyed node was last created with.
#[doc(hidden)]
pub struct Inputs(Vec<Option<Box<dyn Any + Send + Sync>>>);

impl Inputs {
    /// Keeps a copy of each probed value. `Inputs::of(&[&__lithia_p0])`
    pub fn of(probes: &[&dyn Probe]) -> Inputs {
        Inputs(probes.iter().map(|probe: &&dyn Probe| probe.keep()).collect())
    }

    /// Whether every probe equals what was kept.
    fn matches(&self, probes: &[&dyn Probe]) -> bool {
        self.0.len() == probes.len()
            && self.0.iter().zip(probes).all(|(stored, probe): (&Option<Box<dyn Any + Send + Sync>>, &&dyn Probe)| {
                stored.as_deref().is_some_and(|stored: &(dyn Any + Send + Sync)| probe.matches(stored))
            })
    }
}

/// A node reclaimed by key, typed.
#[doc(hidden)]
pub struct Reused<K: NodeValue>(pub(crate) Arc<Node<K>>);

impl<K: NodeValue> Reused<K> {
    /// A new outside handle to the reclaimed derived node.
    pub fn derived(self) -> DerivedNode<K> {
        DerivedNode::outside(&self.0)
    }

    /// A new outside handle to the reclaimed source node.
    pub fn source(self) -> SourceNode<K> {
        SourceNode::outside(&self.0)
    }
}

/// What a keyed creation found in its creator's holdover.
#[doc(hidden)]
pub enum Reuse<K: NodeValue> {
    /// A node with equal inputs: used as it is.
    Same(Reused<K>),
    /// A node whose inputs changed: it takes the new ones and reruns.
    Changed(Reused<K>),
    /// Nothing to reclaim: a new node is created with the key.
    Fresh,
}

/// What `Options::keyed` carries to node creation.
pub(crate) struct Keyed {
    pub(crate) reclaimed: Option<NodeRef>,
    value: Box<dyn DynKey>,
    pub(crate) inputs: Inputs,
}

impl Keyed {
    pub(crate) fn new<K: NodeValue, T>(reuse: Reuse<K>, key: NodeKey<T>, inputs: Inputs) -> Keyed {
        let reclaimed: Option<NodeRef> = match reuse {
            Reuse::Same(Reused(node)) | Reuse::Changed(Reused(node)) => Some(node),
            Reuse::Fresh => None,
        };
        Keyed { reclaimed, value: key.into_box(), inputs }
    }

    pub(crate) fn key(self, site: &'static Site) -> (Key, Inputs) {
        let hash: u64 = self.value.hash_at(site);
        (Key { hash, value: self.value }, self.inputs)
    }
}

/// Keyed nodes of one creator, by key hash, each list in creation order.
#[derive(Default)]
pub(crate) struct KeyedSet(BTreeMap<u64, Vec<NodeRef>>);

impl KeyedSet {
    pub(crate) fn insert(&mut self, node: NodeRef) {
        let hash: u64 = node.header().key.as_ref().map_or(0, |key: &Key| key.hash);
        self.0.entry(hash).or_default().push(node);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &NodeRef> {
        self.0.values().flatten()
    }

    /// Moves every node of `other` in, after this set's own.
    pub(crate) fn merge(&mut self, other: KeyedSet) {
        for (hash, mut list) in other.0 {
            self.0.entry(hash).or_default().append(&mut list);
        }
    }

    pub(crate) fn drain_into(&mut self, out: &mut Vec<NodeRef>) {
        for (_, list) in std::mem::take(&mut self.0) {
            out.extend(list);
        }
    }

    pub(crate) fn remove(&mut self, id: NodeId) -> Option<NodeRef> {
        let mut found: Option<(u64, usize)> = None;
        for (hash, list) in &self.0 {
            if let Some(at) = list.iter().position(|node: &NodeRef| node.header().id == id) {
                found = Some((*hash, at));
                break;
            }
        }
        let (hash, at): (u64, usize) = found?;
        let list: &mut Vec<NodeRef> = self.0.get_mut(&hash)?;
        let node: NodeRef = list.remove(at);
        if list.is_empty() {
            self.0.remove(&hash);
        }
        Some(node)
    }

    fn contains(&self, key: &KeyRef<'_>) -> bool {
        self.0.get(&key.hash).is_some_and(|list: &Vec<NodeRef>| {
            list.iter().any(|node: &NodeRef| node.header().key.as_ref().is_some_and(|own: &Key| own.matches(key)))
        })
    }

    /// Takes the first node, in creation order, that `fits`.
    fn take(&mut self, key: &KeyRef<'_>, fits: impl Fn(&Header) -> bool) -> Option<NodeRef> {
        let list: &mut Vec<NodeRef> = self.0.get_mut(&key.hash)?;
        let at: usize = list.iter().position(|node: &NodeRef| {
            let header: &Header = node.header();
            header.key.as_ref().is_some_and(|own: &Key| own.matches(key)) && fits(header)
        })?;
        let node: NodeRef = list.remove(at);
        if list.is_empty() {
            self.0.remove(&key.hash);
        }
        Some(node)
    }
}

/// The creator of keyed creations made here: the node whose run is being polled, unless disposed.
fn creator() -> Option<NodeRef> {
    let run: Arc<RunShared> = context::creating_run()?;
    run.node.upgrade()
}

/// Reclaims the creator's held-over node with this key, site, kind, value type and parents, and
/// moves it into the current generation's keyed set (spec §10.1, Keyed nodes).
fn reclaim(key: &KeyRef<'_>, kind: Kind, value_type: TypeId, parent_ids: &[NodeId]) -> Option<NodeRef> {
    let creator: NodeRef = creator()?;
    let header: &Header = creator.header();
    let mut guard: MutexGuard<'_, crate::node::Sched> = header.sched.lock();
    if guard.closed {
        return None;
    }
    let duplicate: bool = guard.keyed.contains(key);
    let site: &'static Site = key.site;
    let found: Option<NodeRef> = guard.holdover.take(key, |candidate: &Header| {
        std::ptr::eq(candidate.site, site)
            && candidate.kind == kind
            && candidate.value_type == value_type
            && !candidate.is_ghost()
            && *candidate.parent_ids == *parent_ids
    });
    if let Some(node) = &found {
        guard.keyed.insert(node.clone());
    }
    drop(guard);
    if duplicate {
        crate::diag::duplicate_key(site);
    }
    found
}

/// Gives a reclaimed node back: its creator's generation lets go of it at once.
pub(crate) fn unclaim(node: &NodeRef) {
    let creator: Option<NodeRef> = creator();
    let released: Option<NodeRef> =
        creator.as_ref().and_then(|creator: &NodeRef| creator.header().sched.lock().keyed.remove(node.header().id));
    if let Some(released) = released {
        crate::support::release_generation(vec![released]);
    }
}

/// Whether a duplicate of `key` was already created by the current run; reported, never refused.
pub(crate) fn note_fresh(creator: &mut crate::node::Sched, node: &NodeRef) -> bool {
    let header: &Header = node.header();
    let Some(key) = header.key.as_ref() else {
        return false;
    };
    let probe: KeyRef<'_> = KeyRef { site: header.site, hash: key.hash, value: &*key.value };
    let duplicate: bool = creator.keyed.contains(&probe);
    creator.keyed.insert(node.clone());
    duplicate
}

pub(crate) fn typed<K: NodeValue>(node: NodeRef) -> Arc<Node<K>> {
    match node.into_any().downcast::<Node<K>>() {
        Ok(node) => node,
        Err(_) => unreachable!("lithia: a reclaimed node of the wrong value type"),
    }
}

fn reuse<K: NodeValue>(key: &KeyRef<'_>, kind: Kind, parent_ids: &[NodeId], probes: &[&dyn Probe]) -> Reuse<K> {
    let Some(node) = reclaim(key, kind, TypeId::of::<K>(), parent_ids) else {
        return Reuse::Fresh;
    };
    let node: Arc<Node<K>> = typed(node);
    let same: bool = if kind == Kind::Source {
        node.value
            .load_full()
            .is_some_and(|current: Arc<K>| probes.iter().all(|probe: &&dyn Probe| probe.matches(&*current)))
    } else {
        node.header.inputs.lock().as_ref().is_some_and(|inputs: &Inputs| inputs.matches(probes))
    };
    if same { Reuse::Same(Reused(node)) } else { Reuse::Changed(Reused(node)) }
}

/// Looks up a keyed derived node; `probes` are its captures. Called by `effect!`.
#[doc(hidden)]
pub fn reuse_derived<K: NodeValue>(key: &KeyRef<'_>, deps: &[NodeHandle], probes: &[&dyn Probe]) -> Reuse<K> {
    let mut parent_ids: Vec<NodeId> = deps.iter().map(|dep: &NodeHandle| dep.0.header().id).collect();
    parent_ids.sort_unstable();
    parent_ids.dedup();
    reuse(key, Kind::Derived, &parent_ids, probes)
}

/// Looks up a keyed async source block; `probes` are its captures. Called by `effect!`.
#[doc(hidden)]
pub fn reuse_source_async<K: NodeValue>(key: &KeyRef<'_>, probes: &[&dyn Probe]) -> Reuse<K> {
    reuse(key, Kind::AsyncSource, &[], probes)
}

/// Looks up a keyed source node; `probe` is its new value. Called by `effect!`.
#[doc(hidden)]
pub fn reuse_source<K: NodeValue>(key: &KeyRef<'_>, probe: &dyn Probe) -> Reuse<K> {
    reuse(key, Kind::Source, &[], &[probe])
}
