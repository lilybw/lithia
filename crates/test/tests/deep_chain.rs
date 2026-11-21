// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! A 20 000-node chain, in a binary of its own: while any scoped node is alive in the process, each
//! sweep walks the chain's handleless nodes to its top, so building it beside other tests is quadratic.

use std::time::Duration;

use lithia::{DerivedNode, SourceNode, effect};

#[lithia::test]
async fn a_long_chain_propagates_and_drops_without_recursion() {
    let source: SourceNode<u64> = effect!(0);
    let mut tail: DerivedNode<u64> = effect!(|source| *source.get() + 1);
    for _ in 0..20_000 {
        let previous: DerivedNode<u64> = tail.clone();
        tail = effect!(|previous| *previous.get() + 1);
    }
    lithia::flush().await;
    assert_eq!(*tail.get(), 20_001);
    source.set(10);
    lithia::flush().await;
    assert_eq!(*tail.get(), 20_011);
    let base: usize = lithia::test::live_nodes().await;
    // Every node is top-level, so support holds the chain while `source` is held. Dropping the
    // head's handle starts one sweep that lets go of all 20 002 nodes; they are freed
    // bottom-up, parents through a worklist and bodies on the blocking pool, without recursion.
    drop(tail);
    assert_eq!(lithia::test::live_nodes().await, base, "support still holds the chain");
    drop(source);
    let freed: Result<(), tokio::time::error::Elapsed> = tokio::time::timeout(Duration::from_secs(15), async {
        while lithia::test::live_nodes().await != base - 20_002 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(freed.is_ok(), "live nodes: {} of {}", lithia::test::live_nodes().await, base - 20_002);
}
