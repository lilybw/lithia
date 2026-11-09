//! Capture mode: handle clones an `effect!` expansion makes for a body are internal handles,
//! and their nodes are recorded for the writer edges of the node being created.

use std::cell::{Cell, RefCell};

use crate::node::NodeRef;

crate::sync::thread_local! {
    static RECORDING: RefCell<Option<Vec<(NodeRef, bool)>>> = RefCell::new(None);
    static READ_ONLY: Cell<bool> = Cell::new(false);
    static MOVED: RefCell<Vec<NodeRef>> = RefCell::new(Vec::new());
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
pub struct Captured(pub(crate) Vec<(NodeRef, bool)>, Vec<NodeRef>);

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
        Captured(self.restore(), moved)
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
            drop(Captured(Vec::new(), moved));
        }
    }
}
