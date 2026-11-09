//! Support: what keeps top-level nodes alive without a handle, and the sweeps that let go of them.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
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
    let now: Result<bool, std::thread::AccessError> = OP.try_with(|op: &RefCell<Op>| {
        let mut op = op.borrow_mut();
        if let Some(node) = node.take() {
            op.starts.push(node);
        }
        op.depth == 0 && !op.running
    });
    match now {
        Ok(true) => run(),
        Ok(false) => {}
        Err(_) => {
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
    !guard.subscribers.is_empty() || !guard.generation.is_empty()
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
    let _lock: MutexGuard<'_, ()> = SWEEP.lock();
    // The region: everything downstream of the starting points, up to anchors, whose support
    // cannot have changed. Outside it, a node support holds still counts as supported.
    let mut region: HashMap<NodeId, NodeRef> = HashMap::new();
    let mut order: Vec<NodeRef> = Vec::new();
    let mut stack: Vec<NodeRef> = starts;
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
    let mut search: Search = Search { region, ..Search::default() };
    for node in &order {
        let header: &Header = node.header();
        if is_held(header)
            && !search.supported(node, &mut keep)
            && let Some(me) = header.support.lock().take()
        {
            crate::diag::support_released(header.site);
            keep.push(me);
        }
    }
    for node in &order {
        unread_check(node);
    }
    keep.extend(order);
    keep.extend(search.region.into_values());
    keep
}

/// Warns about a memo that support keeps running though nothing reads it.
fn unread_check(node: &NodeRef) {
    let header: &Header = node.header();
    if header.is_unit
        || !is_held(header)
        || !header.targets.is_empty()
        || header.anchors.load(Ordering::SeqCst) > 0
        || header.captured.load(Ordering::SeqCst) > 0
    {
        return;
    }
    let readers: bool = header
        .changes
        .lock()
        .as_ref()
        .is_some_and(|sender: &tokio::sync::watch::Sender<u64>| sender.receiver_count() > 0);
    if !readers && header.sched.lock().subscribers.is_empty() {
        crate::diag::unread_memo(header.site);
    }
}

/// Nodes support flows to from `node`: children, its generation, and the nodes it may write.
fn successors(node: &NodeRef, keep: &mut Vec<NodeRef>) -> Vec<NodeRef> {
    let header: &Header = node.header();
    let mut out: Vec<NodeRef> = header.subscribers();
    out.extend(header.sched.lock().generation.iter().cloned());
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

/// Backward reachability from anchors, remembered for the rest of one sweep.
#[derive(Default)]
struct Search {
    supported: HashMap<NodeId, bool>,
    blocked: HashMap<NodeId, bool>,
    /// The walked region; any node support holds outside it counts as supported.
    region: HashMap<NodeId, NodeRef>,
}

impl Search {
    /// Whether an anchor reaches `node` (the least fixpoint of support).
    fn supported(&mut self, node: &NodeRef, keep: &mut Vec<NodeRef>) -> bool {
        let id: NodeId = node.header().id;
        if let Some(known) = self.supported.get(&id) {
            return *known;
        }
        let mut visited: Vec<NodeId> = Vec::new();
        let mut seen: HashSet<NodeId> = HashSet::new();
        let mut queue: VecDeque<NodeRef> = VecDeque::from([node.clone()]);
        let mut found: bool = false;
        while let Some(current) = queue.pop_front() {
            let header: &Header = current.header();
            if !seen.insert(header.id) {
                continue;
            }
            visited.push(header.id);
            let trusted: bool = !self.region.contains_key(&header.id) && is_held(header);
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

    /// Whether `node` depends on a scoped node without handles, directly or through nodes without handles.
    fn blocked(&mut self, node: &NodeRef, keep: &mut Vec<NodeRef>) -> bool {
        if !ANY_SCOPED.load(Ordering::SeqCst) {
            return false;
        }
        let mut stack: Vec<NodeRef> = vec![node.clone()];
        while let Some(top) = stack.last().cloned() {
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
    let released: Option<NodeRef> = creator.as_ref().and_then(|creator: &NodeRef| {
        let mut guard: MutexGuard<'_, crate::node::Sched> = creator.header().sched.lock();
        let at: Option<usize> = guard.generation.iter().position(|member: &NodeRef| member.header().id == header.id);
        at.map(|at: usize| guard.generation.swap_remove(at))
    });
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
