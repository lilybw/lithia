// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Support: what keeps top-level nodes alive without a handle, and the sweeps that let go of them.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::Ordering;

use crate::ids::NodeId;
use crate::node::{Header, NodeRef, WeakNodeRef};
use crate::sync::{Mutex, MutexGuard};

#[cfg(not(lithia_loom))]
static SWEEP: Mutex<()> = Mutex::new(());
#[cfg(lithia_loom)]
loom::lazy_static! {
    static ref SWEEP: Mutex<()> = Mutex::new(());
}

/// Nodes an exact check of a starting point visits before it leaves the question to the walk.
const START_BUDGET: usize = 64;

/// Set once any node is scoped; until then no node can be blocked by a scoped parent.
static ANY_SCOPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Starting points raised on this thread, swept when the outermost operation ends.
struct Op {
    depth: u32,
    running: bool,
    starts: Vec<NodeRef>,
}

crate::sync::thread_local! {
    static OP: RefCell<Op> = RefCell::new(Op { depth: 0, running: false, starts: Vec::new() });
}

/// An operation that drops references; its sweeps run when the outermost one ends.
pub(crate) struct OpGuard(());

/// Enters an operation on this thread.
pub(crate) fn enter() -> OpGuard {
    let _ = OP.try_with(|op: &RefCell<Op>| op.borrow_mut().depth += 1);
    OpGuard(())
}

impl Drop for OpGuard {
    fn drop(&mut self) {
        let outermost: bool = OP
            .try_with(|op: &RefCell<Op>| {
                let mut op = op.borrow_mut();
                op.depth -= 1;
                op.depth == 0 && !op.running
            })
            .unwrap_or(false);
        if outermost {
            run();
        }
    }
}

/// Whether `header`'s node is an anchor: not a ghost, with an anchoring outside handle.
pub(crate) fn is_anchor(header: &Header) -> bool {
    !header.is_ghost() && header.anchors.load(Ordering::SeqCst) > 0
}

/// Whether support currently holds the node.
pub(crate) fn is_held(header: &Header) -> bool {
    header.support.lock().is_some()
}

/// Records that `node` may have lost support; sweeps at once unless an operation is under way.
pub(crate) fn start(node: NodeRef) {
    if !needs_sweep(&node) {
        return;
    }
    let mut node: Option<NodeRef> = Some(node);
    let now: Option<bool> = OP
        .try_with(|op: &RefCell<Op>| {
            let mut op = op.borrow_mut();
            if let Some(node) = node.take() {
                op.starts.push(node);
            }
            op.depth == 0 && !op.running
        })
        .ok();
    match now {
        Some(true) => run(),
        Some(false) => {}
        None => {
            // This thread is shutting down: sweep inline.
            if let Some(node) = node.take() {
                drop(sweep(vec![node]));
            }
        }
    }
}

/// A node that support does not hold and that has no outgoing edges needs no sweep.
fn needs_sweep(node: &NodeRef) -> bool {
    let header: &Header = node.header();
    if is_held(header) || !header.targets.is_empty() {
        return true;
    }
    let guard: MutexGuard<'_, crate::node::Sched> = header.sched.lock();
    !guard.subscribers.is_empty() || guard.holds_creations()
}

/// Sweeps until no starting point is left; references are dropped outside the sweep lock.
fn run() {
    let entered: bool = OP
        .try_with(|op: &RefCell<Op>| {
            let mut op = op.borrow_mut();
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
            let _ = OP.try_with(|op: &RefCell<Op>| op.borrow_mut().running = false);
        }
    }
    let _reset: Reset = Reset;
    loop {
        let starts: Vec<NodeRef> =
            OP.try_with(|op: &RefCell<Op>| std::mem::take(&mut op.borrow_mut().starts)).unwrap_or_default();
        if starts.is_empty() {
            break;
        }
        let dropped: Vec<NodeRef> = sweep(starts);
        drop(dropped);
    }
}

/// Lets go of every node held by support that no anchor reaches any more; returns the
/// references taken, for the caller to drop after the sweep lock is released.
fn sweep(starts: Vec<NodeRef>) -> Vec<NodeRef> {
    let mut keep: Vec<NodeRef> = Vec::new();
    let raised: usize = starts.len();
    let lock: MutexGuard<'_, ()> = SWEEP.lock();
    // A starting point an anchor still reaches loses nothing downstream, since support flows
    // only forward. The check trusts nothing and gives up after a few nodes.
    let mut lost: Vec<NodeRef> = Vec::new();
    let mut still: Vec<NodeRef> = Vec::new();
    for node in starts {
        let mut exact: Search = Search { budget: Some(START_BUDGET), ..Search::default() };
        if exact.supported(&node, &mut keep) {
            still.push(node);
        } else {
            lost.push(node);
        }
    }
    // The region: everything downstream of the starting points that lost support, up to
    // anchors. Outside it, a node support holds still counts as supported, since only nodes
    // downstream of a starting point can have lost support.
    let mut region: BTreeMap<NodeId, NodeRef> = BTreeMap::new();
    let mut order: Vec<NodeRef> = Vec::new();
    let mut stack: Vec<NodeRef> = lost;
    while let Some(node) = stack.pop() {
        let id: NodeId = node.header().id;
        if region.contains_key(&id) {
            keep.push(node);
            continue;
        }
        for next in successors(&node, &mut keep) {
            if !is_anchor(next.header()) && !region.contains_key(&next.header().id) {
                stack.push(next);
            }
        }
        region.insert(id, node.clone());
        order.push(node);
    }
    let walked: usize = order.len();
    let mut search: Search = Search { region, trust: true, ..Search::default() };
    let mut released: usize = 0;
    for node in &order {
        let header: &Header = node.header();
        if is_held(header)
            && !search.supported(node, &mut keep)
            && let Some(me) = header.support.lock().take()
        {
            crate::diag::support_released(header.site);
            keep.push(me);
            released += 1;
        }
    }
    let unread: Vec<&'static crate::Site> =
        still.iter().chain(&order).filter_map(|node: &NodeRef| unread(node.header())).collect();
    keep.extend(still);
    keep.extend(order);
    keep.extend(search.region.into_values());
    drop(lock);
    for site in unread {
        crate::diag::unread_memo(site);
    }
    tracing::trace!(raised, walked, released, "lithia: sweep");
    keep
}

/// Warns if support keeps the node running as a memo that nothing reads; for when it loses a reader.
pub(crate) fn check_unread(header: &Header) {
    if let Some(site) = unread(header) {
        crate::diag::unread_memo(site);
    }
}

/// The site of a memo that support keeps running though nothing reads it, for the warning.
fn unread(header: &Header) -> Option<&'static crate::Site> {
    if header.is_unit
        || !is_held(header)
        || !header.targets.is_empty()
        || header.anchors.load(Ordering::SeqCst) > 0
        || header.captured.load(Ordering::SeqCst) > 0
    {
        return None;
    }
    let readers: bool = header
        .changes
        .lock()
        .as_ref()
        .is_some_and(|sender: &tokio::sync::watch::Sender<u64>| sender.receiver_count() > 0);
    (!readers && header.sched.lock().subscribers.is_empty()).then_some(header.site)
}

/// Nodes support flows to from `node`: children, its generation, and the nodes it may write.
fn successors(node: &NodeRef, keep: &mut Vec<NodeRef>) -> Vec<NodeRef> {
    let header: &Header = node.header();
    let mut out: Vec<NodeRef> = header.subscribers();
    out.extend(header.sched.lock().creations().cloned());
    out.extend(header.targets.iter().filter_map(|weak: &WeakNodeRef| weak.upgrade()));
    keep.extend(out.iter().cloned());
    out
}

/// Nodes support flows from into `node`: parents, writers, and the run that holds it.
fn predecessors(node: &NodeRef, keep: &mut Vec<NodeRef>) -> Vec<NodeRef> {
    let header: &Header = node.header();
    let mut out: Vec<NodeRef> = parents(node);
    {
        let mut writers: MutexGuard<'_, Vec<WeakNodeRef>> = header.writers.lock();
        writers.retain(|weak: &WeakNodeRef| match weak.upgrade() {
            Some(writer) => {
                out.push(writer);
                true
            }
            None => false,
        });
    }
    if let Some(creator) = header.held_by.lock().as_ref().and_then(WeakNodeRef::upgrade) {
        out.push(creator);
    }
    keep.extend(out.iter().cloned());
    out
}

/// A copy of the parent list, taken under its own lock and never beside a scheduling lock.
fn parents(node: &NodeRef) -> Vec<NodeRef> {
    node.header().parents.lock().clone().unwrap_or_default()
}

/// Backward reachability from anchors, remembered for the rest of one sweep. Ordered maps keep
/// the order of drops, and so loom models, deterministic.
#[derive(Default)]
struct Search {
    supported: BTreeMap<NodeId, bool>,
    blocked: BTreeMap<NodeId, bool>,
    /// The walked region; with `trust`, any node support holds outside it counts as supported.
    region: BTreeMap<NodeId, NodeRef>,
    trust: bool,
    /// Nodes left to visit; once spent, every answer is "unsupported".
    budget: Option<usize>,
}

impl Search {
    /// Whether an anchor reaches `node` (the least fixpoint of support).
    fn supported(&mut self, node: &NodeRef, keep: &mut Vec<NodeRef>) -> bool {
        let id: NodeId = node.header().id;
        if let Some(known) = self.supported.get(&id) {
            return *known;
        }
        let mut visited: Vec<NodeId> = Vec::new();
        let mut seen: BTreeSet<NodeId> = BTreeSet::new();
        let mut queue: VecDeque<NodeRef> = VecDeque::from([node.clone()]);
        let mut found: bool = false;
        while let Some(current) = queue.pop_front() {
            let header: &Header = current.header();
            if !seen.insert(header.id) {
                continue;
            }
            visited.push(header.id);
            if !self.spend() {
                found = false;
                break;
            }
            let trusted: bool = self.trust && !self.region.contains_key(&header.id) && is_held(header);
            if is_anchor(header) || (trusted && current.header().id != id) {
                found = true;
            } else if !header.scoped.load(Ordering::SeqCst) && !self.blocked(&current, keep) {
                for before in predecessors(&current, keep) {
                    match self.supported.get(&before.header().id) {
                        Some(true) => found = true,
                        Some(false) => {}
                        None => queue.push_back(before),
                    }
                }
            }
            keep.push(current);
            if found {
                break;
            }
        }
        if found {
            self.supported.insert(id, true);
        } else {
            for visited in visited {
                self.supported.insert(visited, false);
            }
        }
        found
    }

    /// Takes one visit from the budget; false once it is spent.
    fn spend(&mut self) -> bool {
        match &mut self.budget {
            Some(0) => false,
            Some(left) => {
                *left -= 1;
                true
            }
            None => true,
        }
    }

    /// Whether `node` depends on a scoped node without handles, directly or through nodes without handles.
    fn blocked(&mut self, node: &NodeRef, keep: &mut Vec<NodeRef>) -> bool {
        // Loom models always take the full path, so models running side by side stay deterministic.
        if !cfg!(lithia_loom) && !ANY_SCOPED.load(Ordering::SeqCst) {
            return false;
        }
        let mut stack: Vec<NodeRef> = vec![node.clone()];
        while let Some(top) = stack.last().cloned() {
            if !self.spend() {
                // Spent: counting the node as blocked keeps the answer "unsupported".
                return true;
            }
            let header: &Header = top.header();
            if self.blocked.contains_key(&header.id) {
                stack.pop();
                continue;
            }
            if is_anchor(header) {
                self.blocked.insert(header.id, false);
                stack.pop();
                continue;
            }
            let list: Vec<NodeRef> = parents(&top);
            let direct: bool = list.iter().any(|parent: &NodeRef| {
                let parent: &Header = parent.header();
                (parent.scoped.load(Ordering::SeqCst) && !is_anchor(parent))
                    || self.blocked.get(&parent.id).copied().unwrap_or(false)
            });
            let unresolved: Vec<NodeRef> = list
                .iter()
                .filter(|parent: &&NodeRef| !self.blocked.contains_key(&parent.header().id))
                .cloned()
                .collect();
            keep.extend(list);
            if direct {
                self.blocked.insert(header.id, true);
                stack.pop();
            } else if unresolved.is_empty() {
                self.blocked.insert(header.id, false);
                stack.pop();
            } else {
                stack.extend(unresolved);
            }
        }
        self.blocked.get(&node.header().id).copied().unwrap_or(false)
    }
}

/// Marks `node` scoped: support and its run's generation let go of it, and only its own
/// handles support it from now on.
pub(crate) fn scope(node: &NodeRef) {
    let _op: OpGuard = enter();
    let header: &Header = node.header();
    ANY_SCOPED.store(true, Ordering::SeqCst);
    header.scoped.store(true, Ordering::SeqCst);
    let me: Option<NodeRef> = header.support.lock().take();
    if me.is_some() {
        crate::diag::support_released(header.site);
    }
    let creator: Option<NodeRef> = header.held_by.lock().take().and_then(|weak: WeakNodeRef| weak.upgrade());
    let released: Option<NodeRef> =
        creator.as_ref().and_then(|creator: &NodeRef| creator.header().sched.lock().remove_creation(header.id));
    start(node.clone());
    drop(released);
    drop(creator);
    drop(me);
}

/// Releases a run's generation: each member loses its creation edge and is a starting point.
pub(crate) fn release_generation(members: Vec<NodeRef>) {
    if members.is_empty() {
        return;
    }
    let _op: OpGuard = enter();
    for member in members {
        *member.header().held_by.lock() = None;
        start(member);
    }
}
