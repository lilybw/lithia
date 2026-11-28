// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! `lithia::test::settled` and `lithia::test::live_nodes`. Unstable: may change in any release.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::sync::futures::Notified;

use crate::Readable;
use crate::context;
use crate::graph::epoch::Scope;
use crate::graph::ids::NodeId;
use crate::graph::node::{Header, NodeRef, WeakNodeRef};
use crate::testing::progress::{self, SettleSeq};

/// Delivers the pending batch, then waits until every run in the node's subtree, which follows
/// its runs' creations and writes, has ended. Unstable. `lithia::test::settled(&user).await;`
pub async fn settled<N: Readable>(node: &N) {
    if let Some((batch, target)) = crate::protocol::take_pending() {
        crate::protocol::deliver(&target, batch);
    }
    let root: NodeRef = node.__lithia_node().0;
    let waiter: progress::Waiter = progress::Waiter::new();
    loop {
        let notified: Notified<'_> = waiter.notify().notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let before: SettleSeq = waiter.seq();
        let quiet: bool = subtree_quiet(&root);
        let after: SettleSeq = waiter.seq();
        if quiet && before == after {
            return;
        }
        if before != after {
            continue;
        }
        tokio::select! {
            _ = &mut notified => {}
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
    }
}

/// Number of live nodes created in the caller's scope, once pending payload drops have run.
/// Exact once the scope has settled. `assert_eq!(lithia::test::live_nodes().await, before);`
pub async fn live_nodes() -> usize {
    let scope: Arc<Scope> = context::current_scope();
    let mut quiet: u32 = 0;
    for _ in 0..10_000 {
        if crate::lifetime::drops::queued() == 0 {
            quiet += 1;
            if quiet >= 3 {
                break;
            }
        } else {
            quiet = 0;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    scope.live.load(Ordering::SeqCst)
}

/// Whether `root`, its descendants and the nodes their runs created or wrote have no work left,
/// checked parents first so an idle parent has already delivered to its children.
fn subtree_quiet(root: &NodeRef) -> bool {
    let mut closure: BTreeMap<NodeId, NodeRef> = BTreeMap::new();
    let mut stack: Vec<NodeRef> = vec![root.clone()];
    while let Some(node) = stack.pop() {
        let header: &Header = node.header();
        if closure.contains_key(&header.id) {
            continue;
        }
        let reached: Vec<NodeRef> = header.lock_sched().trace.reached();
        stack.extend(header.subscribers());
        stack.extend(reached);
        stack.extend(header.targets.iter().filter_map(WeakNodeRef::upgrade));
        closure.insert(header.id, node);
    }
    closure.values().all(|node: &NodeRef| node.header().lock_sched().is_quiet())
}
