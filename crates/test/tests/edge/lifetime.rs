// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Reference counting, disposal and ghosts (spec §10).

use std::sync::Arc;
use std::time::Duration;

use lithia::{Changes, DerivedNode, SourceNode, Status, effect};

#[lithia::test]
async fn changes_end_when_the_node_is_freed() {
    let n: SourceNode<u32> = effect!(1);
    let mut stream: Changes<u32> = n.changes();
    assert_eq!(*stream.next().await.unwrap(), 1);
    drop(n);
    assert!(stream.next().await.is_none());
}

#[lithia::test]
async fn changes_on_a_ghost_end_at_once() {
    let n: SourceNode<u32> = effect!(1, ghost = ON_GHOST_EMIT_LATEST);
    lithia::dispose(&n);
    let mut stream: Changes<u32> = n.changes();
    assert!(stream.next().await.is_none());
    assert_eq!(*n.get(), 1);
}

#[lithia::test]
async fn children_of_an_emit_latest_ghost_keep_working() {
    let frozen: SourceNode<i32> = effect!(5, ghost = ON_GHOST_EMIT_LATEST);
    let k: SourceNode<i32> = effect!(1);
    let child: DerivedNode<i32> = effect!(|frozen, k| *frozen.get() + *k.get());
    lithia::flush().await;
    lithia::dispose(&frozen);
    lithia::flush().await;
    assert!(matches!(lithia::status(&child), Status::Ready));
    k.set(10);
    lithia::flush().await;
    assert_eq!(*child.get(), 15);
    // A new child behaves as the surviving one does.
    let late: DerivedNode<i32> = effect!(|frozen, k| *frozen.get() * *k.get());
    lithia::flush().await;
    assert_eq!(*late.get(), 50);
    k.set(2);
    lithia::flush().await;
    assert_eq!((*child.get(), *late.get()), (7, 10));
    // With only the ghost for a parent, it is disposed at once, as disposal would have done.
    let orphan: DerivedNode<i32> = effect!(|frozen| *frozen.get());
    assert!(matches!(lithia::status(&orphan), Status::Ghost));
    lithia::flush().await;
    assert!(orphan.try_get().is_err());
}

#[lithia::test]
async fn a_cause_failure_ghost_fails_fragile_children() {
    let doomed: SourceNode<i32> = effect!(1);
    let k: SourceNode<i32> = effect!(1);
    let strict: DerivedNode<i32> = effect!(|doomed, k| *k.get(), fragility = FRAGILE);
    let lenient: DerivedNode<i32> = effect!(|doomed, k| *k.get());
    lithia::flush().await;
    lithia::dispose(&doomed);
    lithia::flush().await;
    match lithia::status(&strict) {
        Status::Failed(failure) => {
            assert_eq!(failure.message, "parent failed");
            assert_eq!(failure.causes.len(), 1);
            assert_eq!(failure.causes[0].message, "parent disposed");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(matches!(lithia::status(&lenient), Status::Ready), "the body never reads the ghost");
    // New children behave as the surviving ones do.
    let late_strict: DerivedNode<i32> = effect!(|doomed, k| *k.get(), fragility = FRAGILE);
    let late_lenient: DerivedNode<i32> = effect!(|doomed, k| *k.get());
    let late_reader: DerivedNode<i32> = effect!(|doomed, k| *doomed.get() + *k.get());
    lithia::flush().await;
    match lithia::status(&late_strict) {
        Status::Failed(failure) => assert_eq!(failure.causes[0].message, "parent disposed"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(matches!(lithia::status(&late_lenient), Status::Ready));
    assert!(matches!(lithia::status(&late_reader), Status::Failed(_)), "reading the ghost panics");
}

#[lithia::test]
async fn live_nodes_counts_disposals_and_drops() {
    let base: usize = lithia::test::live_nodes().await;
    let root: SourceNode<u32> = effect!(1);
    let a: DerivedNode<u32> = effect!(|root| *root.get());
    let b: DerivedNode<u32> = effect!(|a| *a.get());
    lithia::flush().await;
    assert_eq!(lithia::test::live_nodes().await, base + 3);
    lithia::dispose(&root);
    lithia::dispose(&root);
    assert_eq!(lithia::test::live_nodes().await, base);
    assert!(matches!(lithia::status(&b), Status::Ghost));
    drop((a, b));
    assert_eq!(lithia::test::live_nodes().await, base);
}

#[lithia::test]
async fn a_node_created_in_a_body_is_freed_when_the_value_lets_go() {
    let n: SourceNode<u32> = effect!(1);
    let base: usize = lithia::test::live_nodes().await;
    let holder: DerivedNode<Option<SourceNode<u32>>> = effect!(|n| {
        let inner: SourceNode<u32> = effect!(*n.get());
        if *n.get() == 1 { Some(inner) } else { None }
    });
    lithia::flush().await;
    assert_eq!(lithia::test::live_nodes().await, base + 2, "the holder and the node its value keeps");
    n.set(2);
    lithia::flush().await;
    // The replaced value is dropped, and its node with it; the source the second run
    // discarded stays in that run's generation until the holder's next run is decided.
    assert_eq!(lithia::test::live_nodes().await, base + 2, "the holder and its current generation");
    lithia::dispose(&holder);
    assert_eq!(lithia::test::live_nodes().await, base, "disposal releases the generation");
}

#[lithia::test]
async fn effects_held_in_a_value_stop_with_it() {
    let rows: SourceNode<Vec<SourceNode<String>>> = effect!(Vec::new());
    let printed: SourceNode<u32> = effect!(0);
    let _printers: DerivedNode<Vec<DerivedNode<()>>> = effect!(|rows| {
        rows.get()
            .iter()
            .map(|row: &SourceNode<String>| {
                let row: SourceNode<String> = row.clone();
                effect!(|row| {
                    let _ = row.get();
                    printed.update(|n: &u32| n + 1);
                })
            })
            .collect()
    });
    let first: SourceNode<String> = effect!("a".to_string());
    rows.set(vec![first.clone()]);
    lithia::flush().await;
    assert_eq!(*printed.get(), 1);
    rows.set(Vec::new()); // the old effects are dropped with the old value
    lithia::flush().await;
    tokio::time::sleep(Duration::from_millis(30)).await; // the replaced value drops on the blocking pool
    first.set("b".to_string());
    lithia::flush().await;
    assert_eq!(*printed.get(), 1, "the dropped effect no longer reacts");
}

#[lithia::test]
async fn a_returned_node_keeps_its_parents_alive() {
    fn make() -> DerivedNode<Arc<str>> {
        let name: SourceNode<String> = effect!("lithia".to_string());
        effect!(|name| Arc::from(name.get().as_str()))
    }
    let made: DerivedNode<Arc<str>> = make();
    lithia::flush().await;
    assert_eq!(&**made.get(), "lithia");
}
