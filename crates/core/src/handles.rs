//! Node handles, views and the capability traits (spec §3.3).

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::changes::Changes;
use crate::context::{self, ClosureCreations, ClosureGuard};
use crate::node::{Header, Node, NodeRef, phase};
use crate::sync::MutexGuard;
use crate::{GhostPolicy, Ghosted, NodeValue, Status, diag, ids, protocol};

/// Type-erased reference to a node, used by `effect!` and the free functions.
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
pub trait Readable: sealed::Sealed {
    /// The node's value type.
    type Value: NodeValue;
    /// Current value. Panics if the node is a ghost under `ON_GHOST_CAUSE_FAILURE`.
    fn get(&self) -> Arc<Self::Value>;
    /// Current value, or `Err(Ghosted)` where the node's ghost policy would make `get` panic.
    fn try_get(&self) -> Result<Arc<Self::Value>, Ghosted>;
    /// Stream of this node's values from now on; like `tokio::sync::watch`, it may skip values.
    fn changes(&self) -> Changes<Self::Value>;
    #[doc(hidden)]
    fn __lithia_node(&self) -> NodeHandle;
}

/// Write access; implemented by `SourceNode` and `Setter`.
/// `fn reset(n: &impl Writable<Value = u32>) { n.set(0); }`
pub trait Writable: sealed::Sealed {
    /// The node's value type.
    type Value: NodeValue;
    /// Replaces the value; every read sees it immediately. Panics on a ghost.
    fn set(&self, value: Self::Value);
    /// Like `set`, but returns `Err(Ghosted)` instead of panicking.
    fn try_set(&self, value: Self::Value) -> Result<(), Ghosted>;
}

/// May be passed to `lithia::reflow`; implemented by `SourceNode`, `DerivedNode` and `Setter`.
pub trait Reflow: sealed::Sealed {
    #[doc(hidden)]
    fn __lithia_reflow_node(&self) -> NodeHandle;
}

/// May be passed to `lithia::dispose`; implemented by `SourceNode` and `DerivedNode`.
pub trait Dispose: sealed::Sealed {
    #[doc(hidden)]
    fn __lithia_dispose_node(&self) -> NodeHandle;
}

impl<K: NodeValue> Node<K> {
    /// Current value, honouring the ghost policy (spec §10.5).
    pub(crate) fn read(&self) -> Result<Arc<K>, Ghosted> {
        let header: &Header = &self.header;
        if header.is_ghost() {
            if header.ghost == GhostPolicy::CauseFailure {
                return Err(Ghosted);
            }
            diag::ghost_read(header.site, header.ghost);
        }
        self.value.load_full().ok_or(Ghosted)
    }

    pub(crate) fn read_or_panic(&self) -> Arc<K> {
        match self.read() {
            Ok(value) => value,
            Err(Ghosted) => {
                panic!("lithia: read of a disposed node under ON_GHOST_CAUSE_FAILURE ({})", self.header.site)
            }
        }
    }

    /// Applies a write to a source node: a run whose only item is the new value (spec §6.1).
    pub(crate) fn write(self: &Arc<Self>, guarded: bool, compute: impl FnOnce(&K) -> K) -> Result<(), Ghosted> {
        context::assert_not_in_closure("a lithia write");
        let _creations: ClosureCreations = ClosureCreations::new();
        let header: &Header = &self.header;
        let mut replaced: Option<Arc<K>> = None;
        let mut discarded: Option<K> = None;
        let stamp: Option<u64> = {
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
            let equal: bool = match &self.equals {
                Some(equals) => {
                    let _guard: ClosureGuard = ClosureGuard::enter();
                    equals(&current, &next)
                }
                None => false,
            };
            if equal && !header.flags.contains(crate::Flags::ALWAYS_PROPAGATE) {
                discarded = Some(next);
                None
            } else {
                header.runs.fetch_add(1, Ordering::SeqCst);
                header.clear_bits(phase::PENDING | phase::FAILED);
                header.failure.store(None);
                if equal {
                    discarded = Some(next);
                } else {
                    replaced = self.value.swap(Some(Arc::new(next)));
                    header.notify_changes();
                }
                let stamp: u64 = ids::stamp();
                header.version.store(stamp, Ordering::SeqCst);
                Some(stamp)
            }
        };
        if let Some(value) = discarded {
            crate::drops::drop_later(value, header.runtime.as_ref(), true);
        }
        if let Some(old) = replaced {
            crate::drops::drop_value(old, header.runtime.as_ref());
        }
        if let Some(stamp) = stamp {
            let erased: NodeRef = self.clone();
            protocol::kill_run(&erased);
            context::record_write(&erased, stamp);
        }
        Ok(())
    }

    fn ghost_write_panic(&self) -> ! {
        panic!("lithia: write to a disposed node ({})", self.header.site)
    }
}

macro_rules! readable {
    ($ty:ident) => {
        impl<K: NodeValue> sealed::Sealed for $ty<K> {}

        impl<K: NodeValue> Readable for $ty<K> {
            type Value = K;
            fn get(&self) -> Arc<K> {
                self.node.read_or_panic()
            }
            fn try_get(&self) -> Result<Arc<K>, Ghosted> {
                self.node.read()
            }
            fn changes(&self) -> Changes<K> {
                Changes::new(&self.node)
            }
            fn __lithia_node(&self) -> NodeHandle {
                NodeHandle(self.node.clone())
            }
        }

        impl<K: NodeValue> $ty<K> {
            /// Current value. Panics if the node is a ghost under `ON_GHOST_CAUSE_FAILURE`.
            pub fn get(&self) -> Arc<K> {
                self.node.read_or_panic()
            }

            /// Current value, or `Err(Ghosted)` where the ghost policy would make `get` panic.
            pub fn try_get(&self) -> Result<Arc<K>, Ghosted> {
                self.node.read()
            }

            /// Stream of this node's values from now on: the current value first, then each change.
            pub fn changes(&self) -> Changes<K> {
                Changes::new(&self.node)
            }
        }

        impl<K: NodeValue> Clone for $ty<K> {
            fn clone(&self) -> Self {
                $ty { node: self.node.clone() }
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
/// `let count: SourceNode<u32> = effect!(0); count.set(3);`
pub struct SourceNode<K: NodeValue> {
    pub(crate) node: Arc<Node<K>>,
}

/// Node whose body produces its value from its dependencies; `DerivedNode<()>` is an effect.
/// `let doubled: DerivedNode<u32> = effect!(|count| *count.get() * 2);`
#[must_use = "a derived node stops when its last handle drops"]
pub struct DerivedNode<K: NodeValue> {
    pub(crate) node: Arc<Node<K>>,
}

/// Read-only view of a node.
/// `let reader: Accessor<u32> = count.accessor();`
pub struct Accessor<K: NodeValue> {
    pub(crate) node: Arc<Node<K>>,
}

/// Write-only view of a source node.
/// `let writer: Setter<u32> = count.setter();`
pub struct Setter<K: NodeValue> {
    pub(crate) node: Arc<Node<K>>,
}

readable!(SourceNode);
readable!(DerivedNode);
readable!(Accessor);

impl<K: NodeValue> sealed::Sealed for Setter<K> {}

impl<K: NodeValue> Clone for Setter<K> {
    fn clone(&self) -> Self {
        Setter { node: self.node.clone() }
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
            fn set(&self, value: K) {
                if self.node.write(false, move |_: &K| value).is_err() {
                    self.node.ghost_write_panic();
                }
            }
            fn try_set(&self, value: K) -> Result<(), Ghosted> {
                self.node.write(false, move |_: &K| value)
            }
        }

        impl<K: NodeValue> $ty<K> {
            /// Replaces the value; every read sees it immediately. Panics on a ghost.
            pub fn set(&self, value: K) {
                Writable::set(self, value)
            }

            /// Like `set`, but returns `Err(Ghosted)` instead of panicking.
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
    pub fn update(&self, f: impl FnOnce(&K) -> K) {
        if self.try_update(f).is_err() {
            self.node.ghost_write_panic();
        }
    }

    /// Like `update`, but returns `Err(Ghosted)` instead of panicking.
    pub fn try_update(&self, f: impl FnOnce(&K) -> K) -> Result<(), Ghosted> {
        self.node.write(true, f)
    }

    /// Mutates a copy of the value in place and publishes it; requires `K: Clone` only here.
    /// `history.modify(|v: &mut Vec<u32>| v.push(1));`
    pub fn modify(&self, f: impl FnOnce(&mut K))
    where
        K: Clone,
    {
        if self.try_modify(f).is_err() {
            self.node.ghost_write_panic();
        }
    }

    /// Like `modify`, but returns `Err(Ghosted)` instead of panicking.
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
    pub fn accessor(&self) -> Accessor<K> {
        Accessor { node: self.node.clone() }
    }

    /// Write-only view of this node.
    pub fn setter(&self) -> Setter<K> {
        Setter { node: self.node.clone() }
    }
}

impl<K: NodeValue> DerivedNode<K> {
    /// Read-only view of this node.
    pub fn accessor(&self) -> Accessor<K> {
        Accessor { node: self.node.clone() }
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
