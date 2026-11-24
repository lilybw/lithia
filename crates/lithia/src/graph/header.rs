// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! A node's header: its identity, shape and the state every value type shares (spec §5).

use std::any::TypeId;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::sync::{ArcCell, AtomicBool, AtomicU32, AtomicUsize, Mutex};
use tokio::runtime::Handle;

#[allow(unused_imports)]
use super::node::*;
use crate::expand::capture::SharedLease;
use crate::expand::keyed::{Inputs, Key};
use crate::graph::epoch::Scope;
use crate::graph::ids::{Atomic, NodeId, RunId, Stamp, WriterId};
use crate::options::Shape;
use crate::{Failure, Site, Status};

/// Bits of a node's phase word; `Status` derives from it (spec §5.3).
pub(crate) mod phase {
    /// A run is in flight and has produced no item yet.
    pub(crate) const PENDING: u32 = 1;
    /// The last run panicked, fragility failed the node, or its runtime shut down.
    pub(crate) const FAILED: u32 = 2;
    /// The node was disposed.
    pub(crate) const GHOST: u32 = 4;
}

/// What created the node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `effect!(value)`: written directly, no runs of its own.
    Source,
    /// `effect!(async { … })`: a source fed by one async run.
    AsyncSource,
    /// `effect!(|deps| …)`: runs a body on its parents.
    Derived,
}

/// Type-independent part of a node.
pub(crate) struct Header {
    pub(crate) id: NodeId,
    pub(crate) site: &'static Site,
    pub(crate) creator: Option<NodeId>,
    pub(crate) kind: Kind,
    /// Whether the value type is `()`, i.e. the node is an effect.
    pub(crate) is_unit: bool,
    /// The value type, for reclaiming a keyed node.
    pub(crate) value_type: TypeId,
    /// The options that shape its behaviour; part of a keyed node's identity.
    pub(crate) shape: Shape,
    pub(crate) version: Atomic<Stamp>,
    pub(crate) phase: AtomicU32,
    pub(crate) failure: ArcCell<Failure>,
    /// Id of the current run; stale items are discarded.
    pub(crate) runs: Atomic<RunId>,
    /// The run id an async source block may run under; a write or a new block moves `runs` past it.
    pub(crate) block_run: Atomic<RunId>,
    /// Serializes writers; never poisons.
    pub(crate) write: Mutex<()>,
    /// The node's scheduling lock (spec §11.4 step 1).
    pub(crate) sched: Mutex<Sched>,
    /// Strong references to the parents; taken on disposal.
    pub(crate) parents: Mutex<Option<Parents>>,
    /// Parent ids, sorted; a parent's index is its position here.
    pub(crate) parent_ids: Box<[NodeId]>,
    /// Nodes this node's body may write.
    pub(crate) targets: Box<[WeakNodeRef]>,
    /// The identity it writes under in writer sets: its id, or for a node created in a run, its
    /// lineage, which the same creation in the creator's next run shares (spec §7.4).
    pub(crate) writer: WriterId,
    pub(crate) runtime: Option<Handle>,
    /// Whether its body is synchronous, so its runs may take the fast path (spec §11.4, step 4).
    pub(crate) sync_body: bool,
    pub(crate) scope: Arc<Scope>,
    /// The `changes()` channel, made by the first stream.
    pub(crate) changes: Mutex<Option<ChangeSender>>,
    /// Set once a change stream exists, so a change on a node without one takes no lock.
    pub(crate) streamed: AtomicBool,
    /// How long it lives: anchors, support, its generation and lease (spec §10.1).
    pub(crate) lifetime: Lifetime,
    /// A keyed node's key and inputs (spec §10.1, Keyed nodes).
    pub(crate) keyed: KeyedState,
    /// What its diagnostics track (spec §12).
    pub(crate) diag: crate::diag::DiagState,
}

/// What decides how long a node lives (spec §10.1).
pub(crate) struct Lifetime {
    /// Whether the node still counts towards its scope's live nodes.
    pub(crate) counted: AtomicBool,
    /// Outside handles that anchor the node.
    pub(crate) anchors: AtomicUsize,
    /// Handles that bodies captured; readers for the unread-memo warning.
    pub(crate) captured: AtomicUsize,
    /// Self-reference while support holds the node.
    pub(crate) support: Mutex<Option<NodeRef>>,
    /// Support let go of the node, which an anchor's return or a new writer may hold again.
    pub(crate) let_go: AtomicBool,
    /// Set by `.scoped()`: only the node's own handles support it.
    pub(crate) scoped: AtomicBool,
    /// Whether support holds the node while an anchor reaches it: a top-level node with parents.
    pub(crate) supportable: bool,
    /// The node whose run's generation holds this node.
    pub(crate) held_by: Mutex<Option<WeakNodeRef>>,
    /// Nodes whose bodies may write this node.
    pub(crate) writers: Mutex<Vec<WeakNodeRef>>,
    /// The lease of an async source block's `move` captures, ended once the block stops for good.
    pub(crate) lease: Mutex<Option<SharedLease>>,
}

/// A keyed node's identity beyond its shape, and the state its creator decides it by.
pub(crate) struct KeyedState {
    /// The `key` option's value; set at creation, never changed.
    pub(crate) key: Option<Key>,
    /// What a keyed node's body last captured, to compare a reclaiming creation against.
    pub(crate) inputs: Mutex<Option<Inputs>>,
    /// A node its creator has yet to finish deciding: being let go of, in the holdover, or being
    /// reclaimed until its refresh is done (spec §7.2, Creator order).
    pub(crate) undecided: AtomicBool,
}

impl Header {
    pub(crate) fn phase(&self) -> u32 {
        self.phase.load(Ordering::SeqCst)
    }

    pub(crate) fn is_ghost(&self) -> bool {
        self.phase() & phase::GHOST != 0
    }

    /// Sets `set` and clears `clear` in one step, so no reader sees a phase between the two.
    pub(crate) fn transition(&self, set: u32, clear: u32) {
        let update = |bits: u32| Some((bits | set) & !clear);
        let _ = self.phase.fetch_update(Ordering::SeqCst, Ordering::SeqCst, update);
    }

    pub(crate) fn clear_bits(&self, bits: u32) {
        self.phase.fetch_and(!bits, Ordering::SeqCst);
    }

    /// Condition of the node as seen by readers (spec §5.3).
    pub(crate) fn status(&self) -> Status {
        let bits: u32 = self.phase();
        if bits & phase::GHOST != 0 {
            Status::Ghost
        } else if bits & phase::FAILED != 0 {
            match self.failure.load_full() {
                Some(failure) => Status::Failed(failure),
                None => Status::Failed(Arc::new(Failure::new("failed", *self.site))),
            }
        } else if bits & phase::PENDING != 0 {
            Status::Pending
        } else {
            Status::Ready
        }
    }

    /// Enters `Pending` for a starting run and leaves `Failed` in one step; clears a failure record.
    pub(crate) fn begin_run(&self) {
        let update = |bits: u32| Some((bits | phase::PENDING) & !phase::FAILED);
        let before: u32 = match self.phase.fetch_update(Ordering::SeqCst, Ordering::SeqCst, update) {
            Ok(bits) | Err(bits) => bits,
        };
        if before & phase::FAILED != 0 {
            self.failure.store(None);
        }
    }

    /// Leaves `Pending` and `Failed`, as a write does; clears a failure record only if there is one.
    pub(crate) fn clear_pending_failed(&self) {
        if self.phase.fetch_and(!(phase::PENDING | phase::FAILED), Ordering::SeqCst) & phase::FAILED != 0 {
            self.failure.store(None);
        }
    }

    /// Takes the node's scheduling lock (spec §11.4, step 1).
    pub(crate) fn lock_sched(&self) -> SchedGuard<'_> {
        SchedGuard::new(self.sched.lock())
    }

    /// Records a failure and enters `Failed`.
    pub(crate) fn fail(&self, failure: Failure) {
        self.failure.store(Some(Arc::new(failure)));
        self.transition(phase::FAILED, phase::PENDING);
    }

    /// Index of `parent` in this node's parent list.
    pub(crate) fn parent_index(&self, parent: NodeId) -> Option<usize> {
        self.parent_ids.binary_search(&parent).ok()
    }

    /// Strong references to the live children, pruning freed ones.
    pub(crate) fn subscribers(&self) -> Children {
        let children: Children = self.lock_sched().children();
        children
    }

    /// Removes a freed or disposed child from the subscriber list; the last one leaving may leave
    /// a memo unread (spec §12).
    pub(crate) fn unsubscribe(&self, child: NodeId) {
        let mut guard: SchedGuard<'_> = self.lock_sched();
        let last: bool = guard.subscribers.remove(child) && guard.subscribers.is_empty();
        drop(guard);
        if last {
            crate::lifetime::support::check_unread(self);
        }
    }

    /// Notifies `changes()` streams of a new value.
    pub(crate) fn notify_changes(&self) {
        // A stream set this before its first read of the value, which the value cell's lock orders
        // against this change: either that read sees the change or this load sees the stream.
        if !self.streamed.load(Ordering::Acquire) {
            return;
        }
        if let Some(sender) = self.changes.lock().as_ref() {
            sender.send_modify(|seq: &mut ChangeSeq| *seq = seq.wrapping_add(1));
        }
    }
}
