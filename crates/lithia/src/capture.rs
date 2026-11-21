// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Capture mode: handle clones an `effect!` expansion makes for a body are internal handles,
//! and their nodes are recorded for the writer edges of the node being created.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::ids::NodeId;
use crate::node::NodeRef;
use crate::sync::{Mutex, MutexGuard};

crate::sync::thread_local! {
    static RECORDING: RefCell<Option<Vec<(NodeRef, bool)>>> = RefCell::new(None);
    static READ_ONLY: Cell<bool> = Cell::new(false);
    static MOVED: RefCell<Vec<NodeRef>> = RefCell::new(Vec::new());
    static LEASE: RefCell<Option<Arc<Lease>>> = RefCell::new(None);
}

/// The handles one `move` body took over, by node. A one-shot block ends its lease when it is
/// dropped: the handles it moved out and that live on count as outside handles from then on.
pub(crate) struct Lease(Mutex<Leased>);

struct Leased {
    ended: bool,
    /// Live handles per node, and whether they anchor.
    held: Vec<(NodeRef, bool, usize)>,
}

impl Lease {
    /// Counts one more handle of `node` under the lease.
    pub(crate) fn take(&self, node: NodeRef, anchoring: bool) {
        let mut guard: MutexGuard<'_, Leased> = self.0.lock();
        let id: NodeId = node.header().id;
        match guard.held.iter_mut().find(|(held, _, _): &&mut (NodeRef, bool, usize)| held.header().id == id) {
            Some((_, _, count)) => *count += 1,
            None => guard.held.push((node, anchoring, 1)),
        }
    }

    /// A leased handle of `node` drops: false once the lease has ended, when it counts as an outside handle.
    pub(crate) fn release(&self, node: NodeId) -> bool {
        let mut gone: Option<(NodeRef, bool, usize)> = None;
        let running: bool = {
            let mut guard: MutexGuard<'_, Leased> = self.0.lock();
            if !guard.ended
                && let Some(at) =
                    guard.held.iter().position(|(held, _, _): &(NodeRef, bool, usize)| held.header().id == node)
            {
                guard.held[at].2 -= 1;
                if guard.held[at].2 == 0 {
                    gone = Some(guard.held.swap_remove(at));
                }
            }
            !guard.ended
        };
        drop(gone);
        running
    }

    /// Ends the lease: each handle still alive starts counting as an outside handle.
    pub(crate) fn end(&self) {
        let held: Vec<(NodeRef, bool, usize)> = {
            let mut guard: MutexGuard<'_, Leased> = self.0.lock();
            guard.ended = true;
            std::mem::take(&mut guard.held)
        };
        let _op: crate::support::OpGuard = crate::support::enter();
        for (node, anchoring, count) in held {
            let header: &crate::node::Header = node.header();
            // Anchors first, so the node never looks unanchored in between.
            if anchoring {
                crate::support::add_anchors(header, count, || node.clone());
            }
            if header.captured.fetch_sub(count, Ordering::SeqCst) == count {
                crate::support::check_unread(header);
            }
        }
    }
}

/// The lease of the expansion under way on this thread.
pub(crate) fn lease() -> Arc<Lease> {
    LEASE
        .try_with(|lease: &RefCell<Option<Arc<Lease>>>| {
            lease
                .borrow_mut()
                .get_or_insert_with(|| Arc::new(Lease(Mutex::new(Leased { ended: false, held: Vec::new() }))))
                .clone()
        })
        .unwrap_or_else(|_| Arc::new(Lease(Mutex::new(Leased { ended: false, held: Vec::new() }))))
}

fn take_lease() -> Option<Arc<Lease>> {
    LEASE.try_with(|lease: &RefCell<Option<Arc<Lease>>>| lease.borrow_mut().take()).ok().flatten()
}

/// Records an anchoring handle a `move` body took over: its count drops once the new node's
/// edges exist, when the expansion's `Captured` is dropped.
pub(crate) fn moved(node: NodeRef) {
    let _ = MOVED.try_with(|moved: &RefCell<Vec<NodeRef>>| moved.borrow_mut().push(node));
}

/// Whether handle clones on this thread are captures right now.
pub(crate) fn active() -> bool {
    RECORDING
        .try_with(|recording: &RefCell<Option<Vec<(NodeRef, bool)>>>| recording.borrow().is_some())
        .unwrap_or(false)
}

/// Records a captured node; `writes` is false for read-only captures and `Accessor`s.
pub(crate) fn record(node: NodeRef, writes: bool) {
    let read_only: bool = READ_ONLY.try_with(Cell::get).unwrap_or(false);
    let _ = RECORDING.try_with(|recording: &RefCell<Option<Vec<(NodeRef, bool)>>>| {
        if let Some(list) = recording.borrow_mut().as_mut() {
            list.push((node, writes && !read_only));
        }
    });
}

/// Runs `f` with its handle clones recorded as read-only.
pub(crate) fn read_only<T>(f: impl FnOnce() -> T) -> T {
    let previous: bool = READ_ONLY.try_with(|flag: &Cell<bool>| flag.replace(true)).unwrap_or(false);
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            let previous: bool = self.0;
            let _ = READ_ONLY.try_with(|flag: &Cell<bool>| flag.set(previous));
        }
    }
    let _reset: Reset = Reset(previous);
    f()
}

/// The nodes an expansion captured, handed to node creation.
/// `let captured: Captured = session.finish();`
#[doc(hidden)]
#[derive(Default)]
pub struct Captured(pub(crate) Vec<(NodeRef, bool)>, Vec<NodeRef>, pub(crate) Option<Arc<Lease>>);

impl Drop for Captured {
    fn drop(&mut self) {
        // Handles moved into the body stop counting only now, after creation registered its edges.
        let _op: crate::support::OpGuard = crate::support::enter();
        for node in std::mem::take(&mut self.1) {
            if node.header().anchors.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                crate::support::start(node);
            }
        }
    }
}

/// One `effect!` expansion's capture clones; ends capture mode when finished or dropped.
#[doc(hidden)]
pub struct Capturing {
    outer: Option<Vec<(NodeRef, bool)>>,
    done: bool,
}

impl Capturing {
    /// Starts capture mode on this thread.
    pub fn begin() -> Capturing {
        let outer: Option<Vec<(NodeRef, bool)>> = RECORDING
            .try_with(|recording: &RefCell<Option<Vec<(NodeRef, bool)>>>| recording.borrow_mut().replace(Vec::new()))
            .ok()
            .flatten();
        Capturing { outer, done: false }
    }

    /// Makes one capture; `read_only` marks every node it records as read-only.
    pub fn take<T>(&self, read_only: bool, f: impl FnOnce() -> T) -> T {
        if read_only { crate::capture::read_only(f) } else { f() }
    }

    /// Ends capture mode and returns what was recorded.
    pub fn finish(mut self) -> Captured {
        self.done = true;
        let moved: Vec<NodeRef> = MOVED
            .try_with(|moved: &RefCell<Vec<NodeRef>>| std::mem::take(&mut *moved.borrow_mut()))
            .unwrap_or_default();
        Captured(self.restore(), moved, take_lease())
    }

    fn restore(&mut self) -> Vec<(NodeRef, bool)> {
        let outer: Option<Vec<(NodeRef, bool)>> = self.outer.take();
        RECORDING
            .try_with(|recording: &RefCell<Option<Vec<(NodeRef, bool)>>>| {
                std::mem::replace(&mut *recording.borrow_mut(), outer)
            })
            .ok()
            .flatten()
            .unwrap_or_default()
    }
}

impl Drop for Capturing {
    fn drop(&mut self) {
        if !self.done {
            drop(self.restore());
            let moved: Vec<NodeRef> = MOVED
                .try_with(|moved: &RefCell<Vec<NodeRef>>| std::mem::take(&mut *moved.borrow_mut()))
                .unwrap_or_default();
            drop(Captured(Vec::new(), moved, take_lease()));
        }
    }
}
