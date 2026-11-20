// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Reference counting, disposal and ghosts (spec §10).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lithia::{Changes, DerivedNode, SourceNode, Status, effect};

/// A value that counts its drops; every two compare equal.
struct Counted(Arc<AtomicUsize>);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl PartialEq for Counted {
    fn eq(&self, _: &Counted) -> bool {
        true
    }
}

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

#[lithia::test]
async fn a_creator_decides_its_children_before_they_run() {
    let input: SourceNode<u32> = effect!(0);
    let seen: Arc<std::sync::Mutex<Vec<(u32, u32)>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _creator: DerivedNode<()> = effect!(|input| {
        let made_at: u32 = *input.get();
        // Rebuilt on every run: the old child must not see the input that replaces it.
        effect!(|input| seen.lock().unwrap().push((*made_at, *input.get())));
    });
    lithia::flush().await;
    for value in 1..=3u32 {
        input.set(value);
        lithia::flush().await;
    }
    lithia::test::settled(&input).await;
    let runs: Vec<(u32, u32)> = seen.lock().unwrap().clone();
    assert!(runs.iter().all(|(made_at, value): &(u32, u32)| made_at == value), "{runs:?}");
}

#[lithia::test]
async fn a_keyed_child_never_runs_with_the_captures_its_creator_replaces() {
    let input: SourceNode<u32> = effect!(0);
    let seen: Arc<std::sync::Mutex<Vec<(u32, u32)>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _creator: DerivedNode<()> = effect!(|input| {
        let label: u32 = *input.get();
        effect!(|input| seen.lock().unwrap().push((*label, *input.get())), key = 0u8);
    });
    lithia::flush().await;
    for value in 1..=3u32 {
        input.set(value);
        lithia::flush().await;
    }
    lithia::test::settled(&input).await;
    let runs: Vec<(u32, u32)> = seen.lock().unwrap().clone();
    assert!(runs.iter().all(|(label, value): &(u32, u32)| label == value), "{runs:?}");
}

#[lithia::test]
async fn replaced_values_from_one_step_all_drop_once() {
    // More nodes than one gathered parcel holds, all replaced in the same propagation.
    const NODES: usize = 150;
    let dropped: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    let tick: SourceNode<u64> = effect!(0);
    let nodes: Vec<DerivedNode<Counted>> = (0..NODES)
        .map(|_| {
            let counter: Arc<AtomicUsize> = dropped.clone();
            effect!(
                |tick| {
                    let _ = tick.get();
                    Counted(counter.clone())
                },
                init = Counted(Arc::new(AtomicUsize::new(0)))
            )
        })
        .collect();
    lithia::flush().await;
    for round in 1..=3 {
        tick.set(round);
        lithia::flush().await;
    }
    let _ = lithia::test::live_nodes().await; // waits for queued drops
    assert_eq!(dropped.load(Ordering::SeqCst), 3 * NODES, "each replaced value dropped once");
    drop(nodes);
    drop(tick); // support held the nodes through their parent until now
    let _ = lithia::test::live_nodes().await;
    assert_eq!(dropped.load(Ordering::SeqCst), 4 * NODES, "the last values go with their nodes");
}

#[lithia::test]
async fn values_equals_discards_drop_once() {
    let dropped: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    let kept: SourceNode<Counted> = effect!(Counted(dropped.clone()), equals = lithia::eq);
    for _ in 0..100 {
        kept.set(Counted(dropped.clone())); // equal: discarded, and dropped off the writing thread
    }
    let plain: SourceNode<u64> = effect!(5, equals = lithia::eq);
    for _ in 0..100 {
        plain.set(5); // no drop glue: dropped at once
    }
    let _ = lithia::test::live_nodes().await;
    assert_eq!(dropped.load(Ordering::SeqCst), 100);
    assert_eq!(*plain.get(), 5);
    drop(kept);
    let _ = lithia::test::live_nodes().await;
    assert_eq!(dropped.load(Ordering::SeqCst), 101);
}

#[lithia::test]
async fn a_stream_started_late_or_across_disposal_sees_the_right_values() {
    let n: SourceNode<u32> = effect!(1);
    for value in 2..=5 {
        n.set(value); // no stream yet: nothing to notify
    }
    let mut early: Changes<u32> = n.changes();
    assert_eq!(*early.next().await.unwrap(), 5, "a first stream starts at the current value");
    n.set(6);
    assert_eq!(*early.next().await.unwrap(), 6);
    let mut second: Changes<u32> = n.changes();
    assert_eq!(*second.next().await.unwrap(), 6);
    lithia::dispose(&n);
    assert!(early.next().await.is_none(), "disposal ends a stream started before it");
    assert!(second.next().await.is_none());
    assert!(n.changes().next().await.is_none(), "a stream started after it ends at once");
}
