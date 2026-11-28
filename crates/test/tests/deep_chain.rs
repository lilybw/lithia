// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! A 20 000-node chain, built beside a live scoped node, in a binary of its own for its size: each sweep
//! checks only the nodes downstream of where it starts for a scoped parent, so building stays linear.

use std::time::Duration;

use lithia::{DerivedNode, SourceNode, node};

#[lithia::test]
async fn a_long_chain_propagates_and_drops_without_recursion() {
    let other: SourceNode<u64> = node!(0);
    let _scoped: DerivedNode<()> = node!(|other| {
        let _ = other.get();
    })
    .scoped();
    let source: SourceNode<u64> = node!(0);
    let mut tail: DerivedNode<u64> = node!(|source| *source.get() + 1);
    for _ in 0..20_000 {
        let previous: DerivedNode<u64> = tail.clone();
        tail = node!(|previous| *previous.get() + 1);
    }
    lithia::flush().await;
    assert_eq!(*tail.get(), 20_001);
    source.set(10);
    lithia::flush().await;
    assert_eq!(*tail.get(), 20_011);
    let base: usize = lithia::test::live_nodes().await;
    // Every node is top-level, so support holds the chain while `source` is held. Dropping the
    // head's handle starts one sweep that lets go of all 20 002 nodes; they are freed
    // bottom-up, parents through a worklist and bodies on the drop threads, without recursion.
    drop(tail);
    assert_eq!(lithia::test::live_nodes().await, base, "support still holds the chain");
    // The last anchor goes on a thread with a small stack: freeing 20 002 nodes recursively would overflow it.
    let runtime: tokio::runtime::Handle = tokio::runtime::Handle::current();
    std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || {
            let _entered: tokio::runtime::EnterGuard<'_> = runtime.enter();
            drop(source);
        })
        .expect("a thread")
        .join()
        .expect("the chain is let go of without overflowing the stack");
    let freed: Result<(), tokio::time::error::Elapsed> = tokio::time::timeout(Duration::from_secs(15), async {
        while lithia::test::live_nodes().await != base - 20_002 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(freed.is_ok(), "live nodes: {} of {}", lithia::test::live_nodes().await, base - 20_002);
}
