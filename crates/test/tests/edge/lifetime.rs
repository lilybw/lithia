// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Reference counting, disposal and ghosts (spec §10).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lithia::{Changes, DerivedNode, SourceNode, Status, node};

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
    let n: SourceNode<u32> = node!(1);
    let mut stream: Changes<u32> = n.changes();
    assert_eq!(*stream.next().await.unwrap(), 1);
    drop(n);
    assert!(stream.next().await.is_none());
}

#[lithia::test]
async fn changes_on_a_ghost_end_at_once() {
    let n: SourceNode<u32> = node!(1, disposed = MASK);
    lithia::dispose(&n);
    let mut stream: Changes<u32> = n.changes();
    assert!(stream.next().await.is_none());
    assert_eq!(*n.get(), 1);
}

#[lithia::test]
async fn children_of_a_masked_ghost_keep_working() {
    let frozen: SourceNode<i32> = node!(5, disposed = MASK);
    let k: SourceNode<i32> = node!(1);
    let child: DerivedNode<i32> = node!(|frozen, k| *frozen.get() + *k.get());
    lithia::flush().await;
    lithia::dispose(&frozen);
    lithia::flush().await;
    assert!(matches!(lithia::status(&child), Status::Ready));
    k.set(10);
    lithia::flush().await;
    assert_eq!(*child.get(), 15);
    // A new child behaves as the surviving one does.
    let late: DerivedNode<i32> = node!(|frozen, k| *frozen.get() * *k.get());
    lithia::flush().await;
    assert_eq!(*late.get(), 50);
    k.set(2);
    lithia::flush().await;
    assert_eq!((*child.get(), *late.get()), (7, 10));
    // With only the ghost for a parent, it is disposed at once, as disposal would have done.
    let orphan: DerivedNode<i32> = node!(|frozen| *frozen.get());
    assert!(matches!(lithia::status(&orphan), Status::Ghost));
    lithia::flush().await;
    assert!(orphan.try_get().is_err());
}

#[lithia::test]
async fn a_failing_ghost_fails_non_resilient_children() {
    let doomed: SourceNode<i32> = node!(1);
    let k: SourceNode<i32> = node!(1);
    let strict: DerivedNode<i32> = node!(|doomed, k| *k.get(), resilience = NONE);
    let lenient: DerivedNode<i32> = node!(|doomed, k| *k.get());
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
    let late_strict: DerivedNode<i32> = node!(|doomed, k| *k.get(), resilience = NONE);
    let late_lenient: DerivedNode<i32> = node!(|doomed, k| *k.get());
    let late_reader: DerivedNode<i32> = node!(|doomed, k| *doomed.get() + *k.get());
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
    let root: SourceNode<u32> = node!(1);
    let a: DerivedNode<u32> = node!(|root| *root.get());
    let b: DerivedNode<u32> = node!(|a| *a.get());
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
    let n: SourceNode<u32> = node!(1);
    let base: usize = lithia::test::live_nodes().await;
    let holder: DerivedNode<Option<SourceNode<u32>>> = node!(|n| {
        let inner: SourceNode<u32> = node!(*n.get());
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
    let rows: SourceNode<Vec<SourceNode<String>>> = node!(Vec::new());
    let printed: SourceNode<u32> = node!(0);
    let _printers: DerivedNode<Vec<DerivedNode<()>>> = node!(|rows| {
        rows.get()
            .iter()
            .map(|row: &SourceNode<String>| {
                let row: SourceNode<String> = row.clone();
                node!(|row| {
                    let _ = row.get();
                    printed.update(|n: &u32| n + 1);
                })
            })
            .collect()
    });
    let first: SourceNode<String> = node!("a".to_string());
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
        let name: SourceNode<String> = node!("lithia".to_string());
        node!(|name| Arc::from(name.get().as_str()))
    }
    let made: DerivedNode<Arc<str>> = make();
    lithia::flush().await;
    assert_eq!(&**made.get(), "lithia");
}

#[lithia::test]
async fn a_creator_decides_its_children_before_they_run() {
    let input: SourceNode<u32> = node!(0);
    let seen: Arc<std::sync::Mutex<Vec<(u32, u32)>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _creator: DerivedNode<()> = node!(|input| {
        let made_at: u32 = *input.get();
        // Rebuilt on every run: the old child must not see the input that replaces it.
        node!(|input| seen.lock().unwrap().push((*made_at, *input.get())));
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
    let input: SourceNode<u32> = node!(0);
    let seen: Arc<std::sync::Mutex<Vec<(u32, u32)>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _creator: DerivedNode<()> = node!(|input| {
        let label: u32 = *input.get();
        node!(|input| seen.lock().unwrap().push((*label, *input.get())), key = 0u8);
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
    let tick: SourceNode<u64> = node!(0);
    let nodes: Vec<DerivedNode<Counted>> = (0..NODES)
        .map(|_| {
            let counter: Arc<AtomicUsize> = dropped.clone();
            node!(
                |tick| {
                    let _ = tick.get();
                    Counted(counter.clone())
                },
                init = Counted(Arc::new(AtomicUsize::new(0))),
                no_equals
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
    let kept: SourceNode<Counted> = node!(Counted(dropped.clone()), equals = lithia::eq);
    for _ in 0..100 {
        kept.set(Counted(dropped.clone())); // equal: discarded, and dropped off the writing thread
    }
    let plain: SourceNode<u64> = node!(5, equals = lithia::eq);
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
    let n: SourceNode<u32> = node!(1);
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

#[lithia::test]
async fn disposal_ends_a_run_parked_on_a_future_that_never_wakes_it() {
    let base: usize = lithia::test::live_nodes().await;
    let a: SourceNode<u32> = node!(1);
    let waiting: DerivedNode<u32> = node!(async |a| {
        let value: u32 = *a.get();
        std::future::pending::<()>().await;
        value
    });
    lithia::flush().await;
    lithia::dispose(&waiting);
    drop((a, waiting));
    lithia::flush().await;
    let left: Result<(), tokio::time::error::Elapsed> = tokio::time::timeout(Duration::from_secs(2), async {
        while lithia::test::live_nodes().await > base {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(left.is_ok(), "the parked run kept its parent alive");
}

#[lithia::test]
async fn a_ghost_reports_the_status_its_disposed_option_names() {
    let failing: SourceNode<u32> = node!(1);
    let masked: SourceNode<u32> = node!(2, disposed = MASK);
    let pending: SourceNode<u32> = node!(3, disposed = PEND);
    for node in [&failing, &masked, &pending] {
        lithia::dispose(node);
    }
    assert!(matches!(lithia::status(&failing), Status::Ghost));
    assert!(matches!(lithia::status(&masked), Status::Ready));
    assert!(matches!(lithia::status(&pending), Status::Pending));
    assert_eq!((masked.try_set(4), pending.try_set(5)), (Err(lithia::Rejected(4)), Err(lithia::Rejected(5))));
    assert_eq!((*masked.get(), *pending.get()), (2, 3));
}

/// Records the thread it drops on.
struct DropsWhere(Arc<std::sync::Mutex<Vec<std::thread::ThreadId>>>);

impl Drop for DropsWhere {
    fn drop(&mut self) {
        self.0.lock().unwrap().push(std::thread::current().id());
    }
}

#[lithia::test(flavor = "current_thread")]
async fn a_replaced_value_drops_off_the_writing_thread() {
    let threads: Arc<std::sync::Mutex<Vec<std::thread::ThreadId>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let n: SourceNode<DropsWhere> = node!(DropsWhere(threads.clone()));
    n.set(DropsWhere(threads.clone()));
    let same: SourceNode<DropsWhere> =
        node!(DropsWhere(threads.clone()), equals = |_: &DropsWhere, _: &DropsWhere| true);
    same.set(DropsWhere(threads.clone())); // equal: the new value is discarded
    let input: SourceNode<u32> = node!(0);
    let made: Arc<std::sync::Mutex<Vec<std::thread::ThreadId>>> = threads.clone();
    let derived: DerivedNode<DropsWhere> = node!(
        |input| {
            let _ = input.get();
            DropsWhere(made.clone())
        },
        init = DropsWhere(threads.clone())
    );
    lithia::flush().await;
    input.set(1); // the run's item replaces the first one, on this thread: the only worker
    lithia::flush().await;
    let _ = lithia::test::live_nodes().await;
    let here: std::thread::ThreadId = std::thread::current().id();
    let dropped: Vec<std::thread::ThreadId> = threads.lock().unwrap().clone();
    assert_eq!(dropped.len(), 4, "the written-over and the discarded value, the init value and the first item");
    assert!(dropped.iter().all(|thread: &std::thread::ThreadId| *thread != here), "{dropped:?}");
    drop((n, same, derived));
}
