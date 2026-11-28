// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Node handles, views and the capability traits (spec §3.3).

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::changes::Changes;
use crate::context::{self, ClosureCreations, ClosureGuard};
use crate::expand::capture::SharedLease;
use crate::graph::ids::Stamp;
use crate::graph::node::{Commit, Displaced, Header, Kind, Node, NodeRef, TypedNodeRef};
use crate::sync::MutexGuard;
use crate::{Ghosted, NodeValue, Ref, Status, diag, protocol};

/// Type-erased reference to a node, used by `node!` and the free functions.
#[doc(hidden)]
#[derive(Clone)]
pub struct NodeHandle(pub(crate) NodeRef);

impl fmt::Debug for NodeHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeHandle({})", self.0.header().id)
    }
}

mod sealed {
    pub trait Sealed {}
}

/// Read access, required of every dependency; implemented by `SourceNode`, `DerivedNode` and `Accessor`.
/// `fn show(n: &impl Readable<Value = u32>) { println!("{}", n.get()); }`
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be read, so it cannot be a dependency",
    label = "not `Readable`",
    note = "dependencies are nodes or `Accessor`s; a `Setter` is write-only, and being notified of changes would read it"
)]
pub trait Readable: sealed::Sealed + Send + Sync {
    /// The node's value type.
    type Value: NodeValue;
    /// Current value. Panics if the node is a ghost under `disposed = FAIL`.
    fn get(&self) -> Ref<Self::Value>;
    /// Current value, or `Err(Ghosted)` where the node's `disposed` option would make `get` panic.
    fn try_get(&self) -> Result<Ref<Self::Value>, Ghosted>;
    /// Stream of this node's values from now on; like `tokio::sync::watch`, it may skip values.
    fn changes(&self) -> Changes<Self::Value>;
    /// Calls `f` with the current value, borrowed, without taking a reference to it; `f` may not
    /// write nodes. Panics like `get`.
    /// `let label: String = name.with(|name: &String| name.to_uppercase());`
    #[track_caller]
    fn with<R>(&self, f: impl FnOnce(&Self::Value) -> R) -> R
    where
        Self: Sized,
    {
        f(&self.get())
    }
    #[doc(hidden)]
    fn __lithia_node(&self) -> NodeHandle;
    #[doc(hidden)]
    fn __lithia_move_dep(&self, read_only: bool);
}

impl<T: Readable + ?Sized> sealed::Sealed for &T {}

/// A reference reads as its handle does, so a body's captures, which it sees by reference, can be
/// dependencies of the nodes it creates.
#[diagnostic::do_not_recommend]
impl<T: Readable + ?Sized> Readable for &T {
    type Value = T::Value;
    #[track_caller]
    fn get(&self) -> Ref<T::Value> {
        (**self).get()
    }
    fn try_get(&self) -> Result<Ref<T::Value>, Ghosted> {
        (**self).try_get()
    }
    fn changes(&self) -> Changes<T::Value> {
        (**self).changes()
    }
    fn __lithia_node(&self) -> NodeHandle {
        (**self).__lithia_node()
    }
    /// A body that moves a reference moves no handle: the one it points to stays outside, an anchor.
    fn __lithia_move_dep(&self, _read_only: bool) {}
}

/// Write access; implemented by `SourceNode` and `Setter`.
/// `fn reset(n: &impl Writable<Value = u32>) { n.set(0); }`
pub trait Writable: sealed::Sealed + Send + Sync {
    /// The node's value type.
    type Value: NodeValue;
    /// Replaces the value; every read sees it immediately. Panics on a ghost.
    fn set(&self, value: Self::Value);
    /// Like `set`, but returns `Err(Ghosted)` instead of panicking.
    fn try_set(&self, value: Self::Value) -> Result<(), Ghosted>;
}

/// May be passed to `lithia::reflow`; implemented by `SourceNode`, `DerivedNode` and `Setter`.
pub trait Reflow: sealed::Sealed + Send + Sync {
    #[doc(hidden)]
    fn __lithia_reflow_node(&self) -> NodeHandle;
}

/// May be passed to `lithia::dispose`; implemented by `SourceNode` and `DerivedNode`.
pub trait Dispose: sealed::Sealed + Send + Sync {
    #[doc(hidden)]
    fn __lithia_dispose_node(&self) -> NodeHandle;
}

impl<K: NodeValue> Node<K> {
    /// Current value, honouring the `disposed` option (spec §10.5).
    pub(crate) fn read(&self) -> Result<Arc<K>, Ghosted> {
        let header: &Header = &self.header;
        if header.is_ghost() {
            if header.shape.ghost_fails() {
                return Err(Ghosted);
            }
            diag::ghost_read(header.site, header.shape.disposed);
        }
        self.value.load_full().ok_or(Ghosted)
    }

    /// Calls `f` with the current value, borrowed, under a closure guard, so it cannot write.
    #[track_caller]
    pub(crate) fn read_with<R>(&self, f: impl FnOnce(&K) -> R) -> R {
        let header: &Header = &self.header;
        if header.is_ghost() {
            if header.shape.ghost_fails() {
                panic!("lithia: read of a disposed node under `disposed = FAIL` ({})", header.site);
            }
            diag::ghost_read(header.site, header.shape.disposed);
        }
        let _guard: ClosureGuard = ClosureGuard::enter();
        self.value.with(|value: Option<&K>| match value {
            Some(value) => f(value),
            None => panic!("lithia: read of a disposed node under `disposed = FAIL` ({})", header.site),
        })
    }

    #[track_caller]
    pub(crate) fn read_or_panic(&self) -> Arc<K> {
        match self.read() {
            Ok(value) => value,
            Err(Ghosted) => {
                panic!("lithia: read of a disposed node under `disposed = FAIL` ({})", self.header.site)
            }
        }
    }

    /// Applies a write to a source node: a run whose only item is the new value (spec §6.1).
    #[track_caller]
    pub(crate) fn write(self: &Arc<Self>, guarded: bool, compute: impl FnOnce(&K) -> K) -> Result<(), Ghosted> {
        context::assert_not_in_closure("a lithia write");
        let header: &Header = &self.header;
        if context::suppressed(header.id) {
            // A cut writer's rerun: it wrote this node already in the epoch it was cut in.
            return if header.is_ghost() { Err(Ghosted) } else { Ok(()) };
        }
        let _creations: ClosureCreations = ClosureCreations::new();
        let mut displaced: Displaced<K> = Displaced::default();
        let stamp: Option<Stamp> = {
            let _write: MutexGuard<'_, ()> = header.write.lock();
            if header.is_ghost() {
                return Err(Ghosted);
            }
            let Some(current) = self.value.load_full() else {
                return Err(Ghosted);
            };
            let next: K = if guarded {
                let _guard: ClosureGuard = ClosureGuard::enter();
                compute(&current)
            } else {
                compute(&current)
            };
            self.commit(&current, next, Commit::Write, &mut displaced)
        };
        displaced.release(header.runtime.as_ref());
        if let Some(stamp) = stamp {
            let erased: NodeRef = self.clone();
            // A plain source node is never framed or seeded, so it never has a run to stop.
            if header.kind != Kind::Source {
                protocol::kill_run(&erased);
            }
            context::record_write(&erased, stamp);
        }
        Ok(())
    }

    #[track_caller]
    fn ghost_write_panic(&self) -> ! {
        panic!("lithia: write to a disposed node ({})", self.header.site)
    }
}

macro_rules! readable {
    ($ty:ident) => {
        impl<K: NodeValue> sealed::Sealed for $ty<K> {}

        impl<K: NodeValue> Readable for $ty<K> {
            type Value = K;
            #[track_caller]
            fn get(&self) -> Ref<K> {
                Ref::new(self.node.read_or_panic())
            }
            fn try_get(&self) -> Result<Ref<K>, Ghosted> {
                self.node.read().map(Ref::new)
            }
            fn changes(&self) -> Changes<K> {
                Changes::new(&self.node)
            }
            #[track_caller]
            fn with<R>(&self, f: impl FnOnce(&K) -> R) -> R {
                self.node.read_with(f)
            }
            fn __lithia_node(&self) -> NodeHandle {
                NodeHandle(self.node.clone())
            }
            fn __lithia_move_dep(&self, read_only: bool) {
                if read_only { self.__lithia_move_ro() } else { self.__lithia_move() }
            }
        }

        impl<K: NodeValue> $ty<K> {
            /// Current value. Panics if the node is a ghost under `disposed = FAIL`.
            #[track_caller]
            pub fn get(&self) -> Ref<K> {
                Ref::new(self.node.read_or_panic())
            }

            /// Calls `f` with the current value, borrowed; `f` may not write nodes. Panics like `get`.
            #[track_caller]
            pub fn with<R>(&self, f: impl FnOnce(&K) -> R) -> R {
                self.node.read_with(f)
            }

            /// Current value, or `Err(Ghosted)` where the `disposed` option would make `get` panic.
            #[must_use = "a read has no effect; use the value or drop the call"]
            pub fn try_get(&self) -> Result<Ref<K>, Ghosted> {
                self.node.read().map(Ref::new)
            }

            /// Stream of this node's values from now on: the current value first, then each change.
            #[must_use = "a stream does nothing unless polled"]
            pub fn changes(&self) -> Changes<K> {
                Changes::new(&self.node)
            }
        }

        impl<K: NodeValue> Clone for $ty<K> {
            fn clone(&self) -> Self {
                $ty { internal: HandleCount::new(new_ref(&self.node, $ty::<K>::writes())), node: self.node.clone() }
            }
        }

        impl<K: NodeValue> Drop for $ty<K> {
            fn drop(&mut self) {
                drop_ref(&self.node, &self.internal, $ty::<K>::anchoring(&self.node));
            }
        }

        impl<K: NodeValue> $ty<K> {
            /// A new outside handle; anchors the node unless it is an `Accessor` to a plain source node.
            #[allow(dead_code)]
            pub(crate) fn outside(node: &TypedNodeRef<K>) -> Self {
                if $ty::<K>::anchoring(node) {
                    crate::lifetime::support::add_anchors(&node.header, 1, || node.clone());
                }
                $ty { node: node.clone(), internal: HandleCount::new(false) }
            }

            /// Clones the handle for a body, recorded as a capture its body only reads.
            #[doc(hidden)]
            pub fn __lithia_capture_ro(&self) -> Self {
                crate::expand::capture::read_only(|| self.clone())
            }

            /// Flags the handle a `move` body is about to take as internal.
            #[doc(hidden)]
            pub fn __lithia_move(&self) {
                take_moved(&self.node, &self.internal, $ty::<K>::writes(), $ty::<K>::anchoring(&self.node));
            }

            /// Like `__lithia_move`, recorded as a capture its body only reads.
            #[doc(hidden)]
            pub fn __lithia_move_ro(&self) {
                crate::expand::capture::read_only(|| self.__lithia_move())
            }

            /// A field path whose value is this handle.
            #[doc(hidden)]
            pub fn __lithia_is_handle(&self) -> bool {
                true
            }

            /// Records this handle's node as one a body reaching it by a field path may write.
            #[doc(hidden)]
            pub fn __lithia_field(&self) {
                record_field(&self.node, $ty::<K>::writes());
            }
        }

        impl<K: NodeValue> fmt::Debug for $ty<K> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct(stringify!($ty))
                    .field("id", &self.node.header.id)
                    .field("creator", &self.node.header.creator)
                    .field("status", &self.node.header.status())
                    .field("site", &format_args!("{}", self.node.header.site))
                    .finish()
            }
        }
    };
}

/// Node with no dependencies whose value is set directly; cheap to clone, `Send + Sync`.
/// `let count: SourceNode<u32> = node!(0); count.set(3);`
pub struct SourceNode<K: NodeValue> {
    pub(crate) node: TypedNodeRef<K>,
    pub(crate) internal: HandleCount,
}

/// Node whose body produces its value from its dependencies; `DerivedNode<()>` is an effect.
/// `let doubled: DerivedNode<u32> = node!(|count| *count.get() * 2);`
pub struct DerivedNode<K: NodeValue> {
    pub(crate) node: TypedNodeRef<K>,
    pub(crate) internal: HandleCount,
}

/// Read-only view of a node.
/// `let reader: Accessor<u32> = count.accessor();`
pub struct Accessor<K: NodeValue> {
    pub(crate) node: TypedNodeRef<K>,
    pub(crate) internal: HandleCount,
}

/// Write-only view of a source node.
/// `let writer: Setter<u32> = count.setter();`
pub struct Setter<K: NodeValue> {
    pub(crate) node: TypedNodeRef<K>,
    pub(crate) internal: HandleCount,
}

/// How a handle counts: as an outside handle, or as a capture; a capture a `move` block took over
/// holds the block's lease, and counts as an outside handle once the block has ended (spec §4.2).
// A handle's own state, read and written only by whoever owns the handle.
#[allow(clippy::disallowed_types)]
pub(crate) struct HandleCount {
    internal: std::sync::atomic::AtomicBool,
    lease: std::sync::OnceLock<SharedLease>,
}

impl HandleCount {
    #[allow(clippy::disallowed_types)]
    pub(crate) fn new(internal: bool) -> HandleCount {
        HandleCount { internal: std::sync::atomic::AtomicBool::new(internal), lease: std::sync::OnceLock::new() }
    }
}

/// Records a new handle: internal in capture mode, otherwise an outside handle.
fn new_ref<K: NodeValue>(node: &TypedNodeRef<K>, writes: bool) -> bool {
    if crate::expand::capture::active() {
        node.header.lifetime.captured.fetch_add(1, Ordering::SeqCst);
        let erased: NodeRef = node.clone();
        crate::expand::capture::record(erased, writes);
        return true;
    }
    anchor_outside(node, writes)
}

fn anchor_outside<K: NodeValue>(node: &TypedNodeRef<K>, writes: bool) -> bool {
    // `writes` is false only for `Accessor`, which anchors only a node with a body.
    if writes || node.header.kind != Kind::Source {
        crate::lifetime::support::add_anchors(&node.header, 1, || node.clone());
    }
    false
}

/// Records a node a body reaches by a field path, without a handle of its own.
fn record_field<K: NodeValue>(node: &TypedNodeRef<K>, writes: bool) {
    if crate::expand::capture::active() {
        let erased: NodeRef = node.clone();
        crate::expand::capture::record(erased, writes);
    }
}

/// Flags a handle a `move` body takes as internal, under the expansion's lease; its anchoring
/// count drops after creation.
fn take_moved<K: NodeValue>(node: &TypedNodeRef<K>, count: &HandleCount, writes: bool, anchoring: bool) {
    if !crate::expand::capture::active() {
        return;
    }
    let erased: NodeRef = node.clone();
    crate::expand::capture::record(erased.clone(), writes);
    if count.internal.swap(true, Ordering::Relaxed) {
        return;
    }
    node.header.lifetime.captured.fetch_add(1, Ordering::SeqCst);
    let lease: SharedLease = crate::expand::capture::lease();
    lease.take(erased.clone(), anchoring);
    let _ = count.lease.set(lease);
    if anchoring {
        crate::expand::capture::moved(erased);
    }
}

/// Releases a handle; dropping the last anchoring handle starts a sweep, and the last captured
/// one may leave a memo unread (spec §12).
fn drop_ref<K: NodeValue>(node: &TypedNodeRef<K>, count: &HandleCount, anchoring: bool) {
    if count.internal.load(Ordering::Relaxed) {
        let ended: bool = count.lease.get().is_some_and(|lease: &SharedLease| !lease.release(node.header.id));
        if !ended {
            let before: usize = node.header.lifetime.captured.fetch_sub(1, Ordering::SeqCst);
            debug_assert!(before > 0, "lithia: a captured handle dropped a count it did not hold");
            if before == 1 {
                crate::lifetime::support::check_unread(&node.header);
            }
            return;
        }
    }
    let last: bool = anchoring && {
        let before: usize = node.header.lifetime.anchors.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(before > 0, "lithia: a handle dropped an anchor it did not hold");
        before == 1
    };
    if last {
        let erased: NodeRef = node.clone();
        crate::lifetime::support::raise(erased);
    }
}

macro_rules! kind_of {
    ($ty:ident, $writes:expr) => {
        impl<K: NodeValue> $ty<K> {
            fn writes() -> bool {
                $writes
            }

            fn anchoring(node: &TypedNodeRef<K>) -> bool {
                $writes || node.header.kind != Kind::Source
            }
        }
    };
}

kind_of!(SourceNode, true);
kind_of!(DerivedNode, true);
kind_of!(Accessor, false);

macro_rules! handle_id {
    ($ty:ident) => {
        impl<K: NodeValue> crate::expand::keyed::HandleId for $ty<K> {
            fn __lithia_node_id(&self) -> crate::graph::ids::NodeId {
                self.node.header.id
            }
        }
    };
}

handle_id!(SourceNode);
handle_id!(DerivedNode);
handle_id!(Accessor);
handle_id!(Setter);

readable!(SourceNode);
readable!(DerivedNode);
readable!(Accessor);

impl<K: NodeValue> sealed::Sealed for Setter<K> {}

impl<K: NodeValue> Clone for Setter<K> {
    fn clone(&self) -> Self {
        Setter { internal: HandleCount::new(new_ref(&self.node, true)), node: self.node.clone() }
    }
}

impl<K: NodeValue> Drop for Setter<K> {
    fn drop(&mut self) {
        drop_ref(&self.node, &self.internal, true);
    }
}

impl<K: NodeValue> Setter<K> {
    /// Clones the handle for a body; a `Setter` always writes.
    #[doc(hidden)]
    pub fn __lithia_capture_ro(&self) -> Self {
        self.clone()
    }

    /// Flags the handle a `move` body is about to take as internal.
    #[doc(hidden)]
    pub fn __lithia_move(&self) {
        take_moved(&self.node, &self.internal, true, true);
    }

    /// Like `__lithia_move`; a `Setter` always writes.
    #[doc(hidden)]
    pub fn __lithia_move_ro(&self) {
        self.__lithia_move()
    }

    /// A field path whose value is this handle.
    #[doc(hidden)]
    pub fn __lithia_is_handle(&self) -> bool {
        true
    }

    /// Records this handle's node as one a body reaching it by a field path may write.
    #[doc(hidden)]
    pub fn __lithia_field(&self) {
        record_field(&self.node, true);
    }
}

impl<K: NodeValue> fmt::Debug for Setter<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Setter")
            .field("id", &self.node.header.id)
            .field("site", &format_args!("{}", self.node.header.site))
            .finish()
    }
}

macro_rules! writable {
    ($ty:ident) => {
        impl<K: NodeValue> Writable for $ty<K> {
            type Value = K;
            #[track_caller]
            fn set(&self, value: K) {
                if self.node.write(false, move |_: &K| value).is_err() {
                    self.node.ghost_write_panic();
                }
            }
            #[track_caller]
            fn try_set(&self, value: K) -> Result<(), Ghosted> {
                self.node.write(false, move |_: &K| value)
            }
        }

        impl<K: NodeValue> $ty<K> {
            /// Replaces the value; every read sees it immediately. Panics on a ghost.
            #[track_caller]
            pub fn set(&self, value: K) {
                Writable::set(self, value)
            }

            /// Like `set`, but returns `Err(Ghosted)` instead of panicking.
            #[track_caller]
            pub fn try_set(&self, value: K) -> Result<(), Ghosted> {
                Writable::try_set(self, value)
            }
        }
    };
}

writable!(SourceNode);
writable!(Setter);

impl<K: NodeValue> SourceNode<K> {
    /// Replaces the value with `f(&old)` under the node's write lock. Panics on a ghost.
    #[track_caller]
    pub fn update(&self, f: impl FnOnce(&K) -> K) {
        if self.try_update(f).is_err() {
            self.node.ghost_write_panic();
        }
    }

    /// Like `update`, but returns `Err(Ghosted)` instead of panicking.
    #[track_caller]
    pub fn try_update(&self, f: impl FnOnce(&K) -> K) -> Result<(), Ghosted> {
        self.node.write(true, f)
    }

    /// Mutates a copy of the value in place and publishes it; requires `K: Clone` only here.
    /// `history.modify(|v: &mut Vec<u32>| v.push(1));`
    #[track_caller]
    pub fn modify(&self, f: impl FnOnce(&mut K))
    where
        K: Clone,
    {
        if self.try_modify(f).is_err() {
            self.node.ghost_write_panic();
        }
    }

    /// Like `modify`, but returns `Err(Ghosted)` instead of panicking.
    #[track_caller]
    pub fn try_modify(&self, f: impl FnOnce(&mut K)) -> Result<(), Ghosted>
    where
        K: Clone,
    {
        self.node.write(true, move |old: &K| {
            let mut copy: K = old.clone();
            f(&mut copy);
            copy
        })
    }

    /// Read-only view of this node.
    #[must_use = "an accessor dropped at once anchors and lets go of the node for nothing"]
    pub fn accessor(&self) -> Accessor<K> {
        Accessor::outside(&self.node)
    }

    /// Write-only view of this node.
    #[must_use = "a setter dropped at once anchors and lets go of the node for nothing"]
    pub fn setter(&self) -> Setter<K> {
        crate::lifetime::support::add_anchors(&self.node.header, 1, || self.node.clone());
        Setter { node: self.node.clone(), internal: HandleCount::new(false) }
    }

    /// Lets this node live by its handles alone; nodes hanging off it stop with it (spec §10.2).
    /// Marks the node for every holder, for good. `let input: SourceNode<Req> = node!(req).scoped();`
    #[must_use = "a scoped node lives only while its handles do; bind this one"]
    pub fn scoped(self) -> Self {
        let erased: NodeRef = self.node.clone();
        crate::lifetime::support::scope(&erased);
        self
    }
}

impl<K: NodeValue> DerivedNode<K> {
    /// Read-only view of this node.
    #[must_use = "an accessor dropped at once anchors and lets go of the node for nothing"]
    pub fn accessor(&self) -> Accessor<K> {
        Accessor::outside(&self.node)
    }

    /// Lets this node live by its handles alone; nodes hanging off it stop with it (spec §10.2).
    /// Marks the node for every holder, for good. `let price: DerivedNode<f64> = node!(|input| calc(&input.get())).scoped();`
    #[must_use = "a scoped node lives only while its handles do; bind this one"]
    pub fn scoped(self) -> Self {
        let erased: NodeRef = self.node.clone();
        crate::lifetime::support::scope(&erased);
        self
    }
}

macro_rules! capability {
    ($trait:ident, $method:ident, $($ty:ident),+) => {
        $(
            impl<K: NodeValue> $trait for $ty<K> {
                fn $method(&self) -> NodeHandle {
                    NodeHandle(self.node.clone())
                }
            }
        )+
    };
}

capability!(Reflow, __lithia_reflow_node, SourceNode, DerivedNode, Setter);
capability!(Dispose, __lithia_dispose_node, SourceNode, DerivedNode);

impl NodeHandle {
    pub(crate) fn status(&self) -> Status {
        self.0.header().status()
    }
}
