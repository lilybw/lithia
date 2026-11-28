// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! The operation context: steps that drop references nest in one operation per thread, whose sweeps
//! run, and whose freed payloads are queued together, when the outermost one ends (spec §10.1).

use std::cell::RefCell;

use crate::graph::node::{NodeRef, WeakNodeRef};

/// Starting points raised on this thread, and references to drop, handled when the outermost
/// operation ends.
struct OperationState {
    depth: u32,
    running: bool,
    starts: Vec<NodeRef>,
    /// References a walk took, dropped once the caller's locks are released.
    keep: Vec<NodeRef>,
    /// Nodes no creator holds any more, started again once released references are dropped.
    resumes: Vec<WeakNodeRef>,
}

crate::sync::thread_local! {
    static OP: RefCell<OperationState> = const {
        RefCell::new(OperationState { depth: 0, running: false, starts: Vec::new(), keep: Vec::new(), resumes: Vec::new() })
    };
}

/// An operation that drops references; its sweeps run when the outermost one ends, and the
/// payloads they free are queued together once they have run.
pub(crate) struct Operation {
    _drops: crate::lifetime::drops::DropBatch,
}

/// Enters an operation on this thread.
pub(crate) fn enter() -> Operation {
    #[cfg(lithia_loom)]
    crate::exec::model::busy();
    let drops: crate::lifetime::drops::DropBatch = crate::lifetime::drops::gather();
    let _ = OP.try_with(|op: &RefCell<OperationState>| op.borrow_mut().depth += 1);
    Operation { _drops: drops }
}

impl Drop for Operation {
    fn drop(&mut self) {
        let outermost: bool = OP
            .try_with(|op: &RefCell<OperationState>| {
                let mut op: std::cell::RefMut<'_, OperationState> = op.borrow_mut();
                op.depth -= 1;
                op.depth == 0 && !op.running
            })
            .unwrap_or(false);
        if outermost {
            run_sweeps();
        }
        #[cfg(lithia_loom)]
        crate::exec::model::idle();
    }
}

/// Records a starting point for the operation's sweeps, which run at once outside an operation.
/// Returns the node if this thread is shutting down, for the caller to sweep inline.
pub(crate) fn raise(node: NodeRef) -> Option<NodeRef> {
    let mut node: Option<NodeRef> = Some(node);
    let now: Option<bool> = OP
        .try_with(|op: &RefCell<OperationState>| {
            let mut op: std::cell::RefMut<'_, OperationState> = op.borrow_mut();
            if let Some(node) = node.take() {
                op.starts.push(node);
            }
            op.depth == 0 && !op.running
        })
        .ok();
    if now == Some(true) {
        run_sweeps();
    }
    node
}

/// Holds `keep` until the operation under way ends, so it is never dropped under a caller's lock;
/// returns it when no operation is under way.
pub(crate) fn keep_to_end(keep: Vec<NodeRef>) -> Option<Vec<NodeRef>> {
    let mut keep: Option<Vec<NodeRef>> = Some(keep);
    let _ = OP.try_with(|op: &RefCell<OperationState>| {
        let mut op: std::cell::RefMut<'_, OperationState> = op.borrow_mut();
        if (op.depth > 0 || op.running)
            && let Some(keep) = keep.take()
        {
            op.keep.extend(keep);
        }
    });
    keep
}

/// Sweeps until no starting point is left; references are dropped outside the sweep lock.
fn run_sweeps() {
    let entered: bool = OP
        .try_with(|op: &RefCell<OperationState>| {
            let mut op: std::cell::RefMut<'_, OperationState> = op.borrow_mut();
            if op.running {
                false
            } else {
                op.running = true;
                true
            }
        })
        .unwrap_or(false);
    if !entered {
        return;
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            let _ = OP.try_with(|op: &RefCell<OperationState>| op.borrow_mut().running = false);
        }
    }
    let _reset: Reset = Reset;
    loop {
        let (keep, starts, resumes): (Vec<NodeRef>, Vec<NodeRef>, Vec<WeakNodeRef>) = OP
            .try_with(|op: &RefCell<OperationState>| {
                let mut op: std::cell::RefMut<'_, OperationState> = op.borrow_mut();
                (std::mem::take(&mut op.keep), std::mem::take(&mut op.starts), std::mem::take(&mut op.resumes))
            })
            .unwrap_or_default();
        if keep.is_empty() && starts.is_empty() && resumes.is_empty() {
            break;
        }
        drop(keep);
        if !starts.is_empty() {
            drop(crate::lifetime::support::sweep(starts));
        }
        // Last, so that a node the sweeps freed gets no task.
        for weak in resumes {
            if let Some(node) = weak.upgrade() {
                crate::protocol::resume(&node);
            }
        }
    }
}

/// Starts `node` again once the current operation has dropped what it released: no creator gates
/// it now (spec §7.2, Creator order).
pub(crate) fn defer_resume(node: &NodeRef) {
    let weak: WeakNodeRef = std::sync::Arc::downgrade(node);
    let queued: bool = OP
        .try_with(|op: &RefCell<OperationState>| {
            let mut op: std::cell::RefMut<'_, OperationState> = op.borrow_mut();
            op.resumes.push(weak);
            op.depth == 0 && !op.running
        })
        .unwrap_or(false);
    if queued {
        run_sweeps();
    }
}
