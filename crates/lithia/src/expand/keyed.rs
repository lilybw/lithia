// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Keyed creation: a run reclaims the node its creator's previous run created with the same key (spec §10.1).

use std::any::{Any, TypeId};
use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use crate::context;
use crate::graph::ids::NodeId;
use crate::handles::{DerivedNode, NodeHandle, SourceNode};

/// A key's hash, mixed with its site's address; equal keys at one site hash alike.
pub(crate) type KeyHash = u64;
/// Keyed nodes whose keys share one hash.
type Bucket = Vec<NodeRef>;
/// A captured input kept for comparison with the next creation's, type-erased.
type StoredInput = Box<dyn Any + Send + Sync>;
use crate::graph::node::{Header, Kind, Node, NodeRef, TypedNodeRef};
use crate::options::{Equals, Implicit, Options, Shape};
use crate::run::RunControl;
use crate::{NodeValue, Site};

/// A value `node!`'s `key` option accepts.
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
    fn hash_at(&self, site: &'static Site) -> KeyHash;
}

impl<T: KeyValue> DynKey for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn same(&self, other: &dyn DynKey) -> bool {
        other.as_any().downcast_ref::<T>() == Some(self)
    }

    fn hash_at(&self, site: &'static Site) -> KeyHash {
        key_hash(site, self)
    }
}

/// Hashes a key together with its site and type, so keys of different sites never meet.
fn key_hash<T: KeyValue>(site: &'static Site, value: &T) -> KeyHash {
    let mut hasher: DefaultHasher = DefaultHasher::new();
    std::ptr::from_ref(site).addr().hash(&mut hasher);
    TypeId::of::<T>().hash(&mut hasher);
    value.hash(&mut hasher);
    hasher.finish()
}

/// A node's key, kept in its header.
pub(crate) struct Key {
    pub(crate) hash: KeyHash,
    value: Arc<dyn DynKey>,
}

impl Key {
    fn matches(&self, probe: &KeyRef<'_>) -> bool {
        self.hash == probe.hash && self.value.same(probe.value)
    }

    /// The key's value, shared with the lineage it names.
    pub(crate) fn share(&self) -> SharedKey {
        SharedKey(Some(self.value.clone()))
    }
}

/// A key value a lineage keeps; dropped off the protocol, like the node's own key.
pub(crate) struct SharedKey(Option<Arc<dyn DynKey>>);

impl SharedKey {
    /// Whether `key` has this value; runs the key type's `PartialEq`, so never under a lock.
    pub(crate) fn same(&self, key: &Key) -> bool {
        self.0.as_ref().is_some_and(|own: &Arc<dyn DynKey>| own.same(&*key.value))
    }
}

impl Drop for SharedKey {
    fn drop(&mut self) {
        if let Some(value) = self.0.take() {
            crate::lifetime::drops::drop_later(value, None);
        }
    }
}

/// A key value, checked against `KeyValue` once, where the key is written.
/// `let key: NodeKey<u64> = NodeKey::new(row.id);`
#[doc(hidden)]
pub struct NodeKey<T> {
    value: T,
    erase: fn(&T) -> &dyn DynKey,
    shared: fn(T) -> Arc<dyn DynKey>,
}

impl<T> NodeKey<T> {
    /// Wraps a key value.
    pub fn new(value: T) -> NodeKey<T>
    where
        T: KeyValue,
    {
        NodeKey { value, erase: |value: &T| value, shared: |value: T| Arc::new(value) }
    }

    fn into_shared(self) -> Arc<dyn DynKey> {
        (self.shared)(self.value)
    }
}

/// A key looked up before anything is created; borrows the key value.
/// `KeyRef::new(&__LITHIA_SITE, &__lithia_key)`
#[doc(hidden)]
pub struct KeyRef<'a> {
    site: &'static Site,
    hash: KeyHash,
    value: &'a dyn DynKey,
}

impl<'a> KeyRef<'a> {
    /// Hashes the key with its site.
    pub fn new<T>(site: &'static Site, key: &'a NodeKey<T>) -> KeyRef<'a> {
        let value: &'a dyn DynKey = (key.erase)(&key.value);
        KeyRef { site, hash: value.hash_at(site), value }
    }
}

/// One captured value, compared against what a reclaimed node stored (spec §4.7).
#[doc(hidden)]
pub trait Probe {
    /// Whether `stored` holds an equal input.
    fn matches(&self, stored: &(dyn Any + Send + Sync)) -> bool;
    /// What to compare later runs against; `None` when the value cannot be compared.
    fn keep(&self) -> Option<StoredInput>;
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

    fn keep(&self) -> Option<StoredInput> {
        Some(Box::new(self.0))
    }
}

impl<T: PartialEq + Clone + Send + Sync + 'static> Probe for EqProbe<'_, T> {
    fn matches(&self, stored: &(dyn Any + Send + Sync)) -> bool {
        stored.downcast_ref::<T>() == Some(self.0)
    }

    fn keep(&self) -> Option<StoredInput> {
        Some(Box::new(self.0.clone()))
    }
}

impl<T: ?Sized + Send + Sync + 'static> Probe for ArcProbe<'_, T> {
    fn matches(&self, stored: &(dyn Any + Send + Sync)) -> bool {
        stored.downcast_ref::<Arc<T>>().is_some_and(|stored: &Arc<T>| Arc::ptr_eq(stored, self.0))
    }

    fn keep(&self) -> Option<StoredInput> {
        Some(Box::new(self.0.clone()))
    }
}

impl Probe for OpaqueProbe {
    fn matches(&self, _stored: &(dyn Any + Send + Sync)) -> bool {
        false
    }

    fn keep(&self) -> Option<StoredInput> {
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
pub struct Inputs(Vec<Option<StoredInput>>);

impl Inputs {
    /// Keeps a copy of each probed value. `Inputs::of(&[&__lithia_p0])`
    pub fn of(probes: &[&dyn Probe]) -> Inputs {
        Inputs(probes.iter().map(|probe: &&dyn Probe| probe.keep()).collect())
    }

    /// Drops the kept copies off the protocol; with nothing kept, no user code can run and it drops here.
    pub(crate) fn release(self, runtime: Option<&tokio::runtime::Handle>) {
        if !self.0.is_empty() {
            crate::lifetime::drops::drop_later(self, runtime);
        }
    }

    /// Whether every probe equals what was kept.
    fn matches(&self, probes: &[&dyn Probe]) -> bool {
        self.0.len() == probes.len()
            && self.0.iter().zip(probes).all(|(stored, probe): (&Option<StoredInput>, &&dyn Probe)| {
                stored.as_deref().is_some_and(|stored: &(dyn Any + Send + Sync)| probe.matches(stored))
            })
    }
}

/// A node reclaimed by key, typed.
#[doc(hidden)]
pub struct Reclaimed<K: NodeValue>(pub(crate) TypedNodeRef<K>);

impl<K: NodeValue> Reclaimed<K> {
    /// A new outside handle to the reclaimed derived node, which takes the creation's comparator.
    pub fn derived(
        self,
        options: Options<K>,
        implicit: impl FnOnce(Implicit<K>) -> Option<Equals<K>>,
    ) -> DerivedNode<K> {
        self.adopt(options.or_implicit(implicit));
        DerivedNode::outside(&self.0)
    }

    /// A new outside handle to the reclaimed source node, which takes the creation's comparator.
    pub fn source(self, options: Options<K>, implicit: impl FnOnce(Implicit<K>) -> Option<Equals<K>>) -> SourceNode<K> {
        self.adopt(options.or_implicit(implicit));
        SourceNode::outside(&self.0)
    }

    fn adopt(&self, options: Options<K>) {
        if let Some(old) = self.0.adopt_equals(options.equals) {
            crate::lifetime::drops::drop_later(old, self.0.header.runtime.as_ref());
        }
        decided(&self.0);
    }
}

/// Its creator's run has reclaimed `node`, refreshed if need be: it may run again (spec §7.2).
pub(crate) fn decided<K: NodeValue>(node: &TypedNodeRef<K>) {
    node.header.keyed.undecided.store(false, std::sync::atomic::Ordering::SeqCst);
    let erased: NodeRef = node.clone();
    crate::protocol::resume(&erased);
}

/// What a keyed creation found in its creator's holdover.
#[doc(hidden)]
pub enum Reclaim<K: NodeValue> {
    /// A node with equal inputs: used as it is.
    Same(Reclaimed<K>),
    /// A node whose inputs changed: it takes the new ones and reruns.
    Changed(Reclaimed<K>),
    /// Nothing to reclaim: a new node is created with the key.
    Fresh,
}

/// What `Options::keyed` carries to node creation.
pub(crate) struct Keyed {
    pub(crate) reclaimed: Option<NodeRef>,
    value: Arc<dyn DynKey>,
    pub(crate) inputs: Inputs,
}

impl Keyed {
    pub(crate) fn new<K: NodeValue, T>(reclaim_or_create: Reclaim<K>, key: NodeKey<T>, inputs: Inputs) -> Keyed {
        let reclaimed: Option<NodeRef> = match reclaim_or_create {
            Reclaim::Same(Reclaimed(node)) | Reclaim::Changed(Reclaimed(node)) => Some(node),
            Reclaim::Fresh => None,
        };
        Keyed { reclaimed, value: key.into_shared(), inputs }
    }

    pub(crate) fn key(self, site: &'static Site) -> (Key, Inputs) {
        let hash: KeyHash = self.value.hash_at(site);
        (Key { hash, value: self.value }, self.inputs)
    }
}

/// Keyed nodes of one creator, by key hash, each list in creation order.
#[derive(Default)]
pub(crate) struct KeyedSet(BTreeMap<KeyHash, Bucket>);

impl KeyedSet {
    pub(crate) fn insert(&mut self, node: NodeRef) {
        let hash: KeyHash = node.header().keyed.key.as_ref().map_or(0, |key: &Key| key.hash);
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
        let mut found: Option<(KeyHash, usize)> = None;
        for (hash, list) in &self.0 {
            if let Some(at) = list.iter().position(|node: &NodeRef| node.header().id == id) {
                found = Some((*hash, at));
                break;
            }
        }
        let (hash, at): (KeyHash, usize) = found?;
        let list: &mut Vec<NodeRef> = self.0.get_mut(&hash)?;
        let node: NodeRef = list.remove(at);
        if list.is_empty() {
            self.0.remove(&hash);
        }
        Some(node)
    }

    /// The nodes whose key hashes to `hash` and that `fits`, in creation order; compares no keys.
    fn bucket(&self, hash: KeyHash, fits: impl Fn(&Header) -> bool) -> Vec<NodeRef> {
        self.0.get(&hash).map_or_else(Vec::new, |list: &Vec<NodeRef>| {
            list.iter().filter(|node: &&NodeRef| fits(node.header())).cloned().collect()
        })
    }

    /// Takes node `id` from the nodes whose key hashes to `hash`.
    fn take(&mut self, hash: KeyHash, id: NodeId) -> Option<NodeRef> {
        let list: &mut Vec<NodeRef> = self.0.get_mut(&hash)?;
        let at: usize = list.iter().position(|node: &NodeRef| node.header().id == id)?;
        let node: NodeRef = list.remove(at);
        if list.is_empty() {
            self.0.remove(&hash);
        }
        Some(node)
    }
}

/// Whether `node`'s key equals `key`; runs the key type's `PartialEq`, so never under a lock.
fn has_key(node: &NodeRef, key: &KeyRef<'_>) -> bool {
    node.header().keyed.key.as_ref().is_some_and(|own: &Key| own.matches(key))
}

/// The creator of keyed creations made here: the node whose run is being polled, unless disposed.
fn creator() -> Option<NodeRef> {
    let run: Arc<RunControl> = context::creating_run()?;
    run.node.upgrade()
}

/// Reclaims the creator's held-over node with this key, site, kind, value type, shape and parents,
/// and moves it into the current generation's keyed set (spec §10.1, Keyed nodes). Keys are compared
/// with the scheduling lock released, as they run user code; a candidate gone meanwhile is looked for again.
fn reclaim(key: &KeyRef<'_>, kind: Kind, value_type: TypeId, shape: &Shape, parent_ids: &[NodeId]) -> Option<NodeRef> {
    let creator: NodeRef = creator()?;
    let header: &Header = creator.header();
    let site: &'static Site = key.site;
    let fits = |candidate: &Header| -> bool {
        std::ptr::eq(candidate.site, site)
            && candidate.kind == kind
            && candidate.value_type == value_type
            && candidate.shape == *shape
            && !candidate.is_ghost()
            && *candidate.parent_ids == *parent_ids
    };
    loop {
        let (candidates, created): (Vec<NodeRef>, Vec<NodeRef>) = {
            let guard: crate::graph::node::SchedGuard<'_> = header.lock_sched();
            if guard.closed {
                return None;
            }
            (guard.holdover.bucket(key.hash, fits), guard.keyed.bucket(key.hash, |_: &Header| true))
        };
        let duplicate: bool = created.iter().any(|node: &NodeRef| has_key(node, key));
        let chosen: Option<NodeRef> = candidates.into_iter().find(|node: &NodeRef| has_key(node, key));
        let found: Option<NodeRef> = match chosen {
            None => None,
            Some(node) => {
                let mut guard: crate::graph::node::SchedGuard<'_> = header.lock_sched();
                if guard.closed {
                    return None;
                }
                let Some(taken) = guard.holdover.take(key.hash, node.header().id) else {
                    continue;
                };
                guard.keyed.insert(taken.clone());
                Some(taken)
            }
        };
        if duplicate {
            crate::diag::duplicate_key(site);
        }
        return found;
    }
}

/// Gives a reclaimed node back: its creator's generation lets go of it at once.
pub(crate) fn unclaim(node: &NodeRef) {
    let creator: Option<NodeRef> = creator();
    let released: Option<NodeRef> =
        creator.as_ref().and_then(|creator: &NodeRef| creator.header().lock_sched().keyed.remove(node.header().id));
    if let Some(released) = released {
        crate::lifetime::support::release_generation(vec![released]);
    }
}

/// Adds a fresh keyed node to its creator's set; returns the nodes there already whose key hashes
/// alike, for [`is_duplicate`] once the lock is released.
pub(crate) fn note_fresh(creator: &mut crate::graph::node::Sched, node: &NodeRef) -> Vec<NodeRef> {
    let hash: KeyHash = node.header().keyed.key.as_ref().map_or(0, |key: &Key| key.hash);
    let alike: Vec<NodeRef> = creator.keyed.bucket(hash, |_: &Header| true);
    creator.keyed.insert(node.clone());
    alike
}

/// Whether one of `alike` has `node`'s key: a run created it twice; reported, never refused.
pub(crate) fn is_duplicate(node: &NodeRef, alike: &[NodeRef]) -> bool {
    let header: &Header = node.header();
    let Some(key) = header.keyed.key.as_ref() else {
        return false;
    };
    let probe: KeyRef<'_> = KeyRef { site: header.site, hash: key.hash, value: &*key.value };
    alike.iter().any(|other: &NodeRef| has_key(other, &probe))
}

pub(crate) fn typed<K: NodeValue>(node: NodeRef) -> TypedNodeRef<K> {
    match node.into_any().downcast::<Node<K>>() {
        Ok(node) => node,
        Err(_) => unreachable!("lithia: a reclaimed node of the wrong value type"),
    }
}

fn reclaim_or_create<K: NodeValue>(
    key: &KeyRef<'_>,
    kind: Kind,
    shape: &Shape,
    parent_ids: &[NodeId],
    probes: &[&dyn Probe],
) -> Reclaim<K> {
    let Some(node) = reclaim(key, kind, TypeId::of::<K>(), shape, parent_ids) else {
        return Reclaim::Fresh;
    };
    let node: TypedNodeRef<K> = typed(node);
    let same: bool = if kind == Kind::Source {
        node.value
            .load_full()
            .is_some_and(|current: Arc<K>| probes.iter().all(|probe: &&dyn Probe| probe.matches(&*current)))
    } else {
        node.header.keyed.inputs.lock().as_ref().is_some_and(|inputs: &Inputs| inputs.matches(probes))
    };
    if same { Reclaim::Same(Reclaimed(node)) } else { Reclaim::Changed(Reclaimed(node)) }
}

/// Looks up a keyed derived node; `probes` are its captures. Called by `node!`.
#[doc(hidden)]
pub fn reclaim_derived<K: NodeValue>(
    key: &KeyRef<'_>,
    shape: &Shape,
    deps: &[NodeHandle],
    probes: &[&dyn Probe],
) -> Reclaim<K> {
    let mut parent_ids: Vec<NodeId> = deps.iter().map(|dep: &NodeHandle| dep.0.header().id).collect();
    parent_ids.sort_unstable();
    parent_ids.dedup();
    reclaim_or_create(key, Kind::Derived, shape, &parent_ids, probes)
}

/// Looks up a keyed async source block; `probes` are its captures. Called by `node!`.
#[doc(hidden)]
pub fn reclaim_source_async<K: NodeValue>(key: &KeyRef<'_>, shape: &Shape, probes: &[&dyn Probe]) -> Reclaim<K> {
    reclaim_or_create(key, Kind::AsyncSource, shape, &[], probes)
}

/// Looks up a keyed source node; `probe` is its new value. Called by `node!`.
#[doc(hidden)]
pub fn reclaim_source<K: NodeValue>(key: &KeyRef<'_>, shape: &Shape, probe: &dyn Probe) -> Reclaim<K> {
    reclaim_or_create(key, Kind::Source, shape, &[], &[probe])
}
