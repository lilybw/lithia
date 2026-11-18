// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Writes, equality, closures and status transitions (spec §5.3, §6).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lithia::{Changes, DerivedNode, Ghosted, SourceNode, Status, effect};

#[lithia::test]
async fn writes_are_visible_immediately() {
    let n: SourceNode<u32> = effect!(1);
    n.update(|v: &u32| v + 1);
    assert_eq!(*n.get(), 2);
    n.set(7);
    assert_eq!(*n.get(), 7);
    let list: SourceNode<Vec<u32>> = effect!(vec![1]);
    list.modify(|v: &mut Vec<u32>| v.push(2));
    assert_eq!(*list.get(), vec![1, 2]);
}

#[lithia::test]
async fn writes_inside_closures_panic_and_leave_values_unchanged() {
    let a: SourceNode<u32> = effect!(0);
    let b: SourceNode<u32> = effect!(0);
    let nested_write: std::thread::Result<()> = catch_unwind(AssertUnwindSafe(|| {
        a.update(|v: &u32| {
            b.set(1);
            *v + 1
        })
    }));
    assert!(nested_write.is_err());
    assert_eq!((*a.get(), *b.get()), (0, 0));

    let nested_reflow: std::thread::Result<()> = catch_unwind(AssertUnwindSafe(|| {
        a.update(|v: &u32| {
            lithia::reflow(&b);
            *v
        })
    }));
    assert!(nested_reflow.is_err());

    let nested_dispose: std::thread::Result<()> = catch_unwind(AssertUnwindSafe(|| {
        a.modify(|_v: &mut u32| lithia::dispose(&b));
    }));
    assert!(nested_dispose.is_err());
    assert!(matches!(lithia::status(&b), Status::Ready));

    a.set(5); // the write lock does not poison
    assert_eq!(*a.get(), 5);
}

#[lithia::test]
async fn writes_inside_equals_panic() {
    let other: SourceNode<u32> = effect!(0);
    let probe: SourceNode<u32> = other.clone();
    let n: SourceNode<u32> = effect!(
        0,
        equals = move |_old: &u32, _new: &u32| {
            probe.set(9);
            false
        }
    );
    let outcome: std::thread::Result<()> = catch_unwind(AssertUnwindSafe(|| n.set(1)));
    assert!(outcome.is_err());
    assert_eq!((*n.get(), *other.get()), (0, 0));
}

#[lithia::test]
async fn a_panicking_comparator_fails_the_write_and_keeps_the_value() {
    let n: SourceNode<u32> = effect!(
        1,
        equals = |_old: &u32, new: &u32| {
            if *new == 13 {
                panic!("unlucky");
            }
            false
        }
    );
    let outcome: std::thread::Result<()> = catch_unwind(AssertUnwindSafe(|| n.set(13)));
    assert!(outcome.is_err());
    assert_eq!(*n.get(), 1);
    n.set(2);
    assert_eq!(*n.get(), 2);
}

#[lithia::test]
async fn a_panicking_comparator_fails_the_run() {
    let n: SourceNode<u32> = effect!(1);
    let picky: DerivedNode<u32> = effect!(
        |n| *n.get(),
        equals = |_old: &u32, new: &u32| {
            if *new == 13 {
                panic!("unlucky");
            }
            false
        }
    );
    lithia::flush().await;
    assert_eq!(*picky.get(), 1);
    n.set(13);
    lithia::flush().await;
    assert!(matches!(lithia::status(&picky), Status::Failed(_)));
    assert_eq!(*picky.get(), 1);
    n.set(3);
    lithia::flush().await;
    assert!(matches!(lithia::status(&picky), Status::Ready));
    assert_eq!(*picky.get(), 3);
}

#[lithia::test]
async fn nodes_created_inside_update_closures_are_delivered() {
    let n: SourceNode<u32> = effect!(2);
    let holder: SourceNode<Option<DerivedNode<u32>>> = effect!(None);
    let source: SourceNode<u32> = n.clone();
    holder.update(move |_old: &Option<DerivedNode<u32>>| Some(effect!(|source| *source.get() * 10)));
    lithia::flush().await;
    let child: DerivedNode<u32> = (*holder.get()).clone().expect("created in the closure");
    assert_eq!(*child.get(), 20);
    n.set(3);
    lithia::flush().await;
    assert_eq!(*child.get(), 30);
}

#[lithia::test]
async fn try_operations_report_ghosts() {
    let n: SourceNode<u32> = effect!(1);
    lithia::dispose(&n);
    assert_eq!(n.try_get(), Err(Ghosted));
    assert_eq!(n.try_set(2), Err(Ghosted));
    assert_eq!(n.try_update(|v: &u32| v + 1), Err(Ghosted));
    assert_eq!(n.try_modify(|v: &mut u32| *v += 1), Err(Ghosted));
    assert_eq!(lithia::try_reflow(&n), Err(Ghosted));
    let read: std::thread::Result<Arc<u32>> = catch_unwind(AssertUnwindSafe(|| n.get()));
    let message: String = match read {
        Ok(_) => panic!("reading a ghost under ON_GHOST_CAUSE_FAILURE must panic"),
        Err(payload) => payload.downcast_ref::<String>().cloned().unwrap_or_default(),
    };
    assert!(message.contains("values.rs"), "the panic names the site: {message}");
    assert!(catch_unwind(AssertUnwindSafe(|| n.set(3))).is_err());
    assert!(catch_unwind(AssertUnwindSafe(|| lithia::reflow(&n))).is_err());
}

#[lithia::test]
async fn equal_writes_do_not_reach_changes() {
    let n: SourceNode<u32> = effect!(1, equals = lithia::eq);
    let mut stream: Changes<u32> = n.changes();
    assert_eq!(*stream.next().await.unwrap(), 1);
    n.set(1);
    let next: Result<Option<Arc<u32>>, tokio::time::error::Elapsed> =
        tokio::time::timeout(Duration::from_millis(30), stream.next()).await;
    assert!(next.is_err(), "an equal write is skipped");
    n.set(2);
    assert_eq!(*stream.next().await.unwrap(), 2);
}

#[lithia::test]
async fn always_propagate_reruns_children_of_derived_nodes() {
    let a: SourceNode<i32> = effect!(1);
    let parity: DerivedNode<i32> = effect!(|a| *a.get() % 2, equals = lithia::eq, flags = ALWAYS_PROPAGATE);
    let runs: SourceNode<u32> = effect!(0);
    let _child: DerivedNode<()> = effect!(|parity| runs.update(|n: &u32| n + 1));
    lithia::flush().await;
    let before: Arc<i32> = parity.get();
    a.set(3);
    lithia::flush().await;
    assert!(Arc::ptr_eq(&before, &parity.get()));
    assert_eq!(*runs.get(), 2);
}

#[lithia::test]
async fn always_propagate_without_equals_changes_nothing() {
    let flags: lithia::Flags = lithia::ALWAYS_PROPAGATE;
    let literal: SourceNode<u32> = effect!(1, flags = ALWAYS_PROPAGATE);
    let computed: SourceNode<u32> = effect!(1, flags = flags);
    let runs: SourceNode<u32> = effect!(0);
    let _child: DerivedNode<()> = effect!(|literal, computed| {
        let _ = (literal.get(), computed.get());
        runs.update(|n: &u32| n + 1);
    });
    lithia::flush().await;
    literal.set(1);
    lithia::flush().await;
    computed.set(1);
    lithia::flush().await;
    // Without `equals` nothing compares, so every write propagates, flag or not.
    assert_eq!(*runs.get(), 3);
}

#[lithia::test]
async fn reflow_of_a_derived_node_reruns_it_and_its_children() {
    let a: SourceNode<i32> = effect!(1);
    let body_runs: SourceNode<u32> = effect!(0);
    let child_runs: SourceNode<u32> = effect!(0);
    let same: DerivedNode<i32> = effect!(
        |a| {
            body_runs.update(|n: &u32| n + 1);
            *a.get()
        },
        equals = lithia::eq
    );
    let _child: DerivedNode<()> = effect!(|same| child_runs.update(|n: &u32| n + 1));
    lithia::flush().await;
    assert_eq!((*body_runs.get(), *child_runs.get()), (1, 1));
    lithia::reflow(&same);
    lithia::flush().await;
    assert_eq!((*body_runs.get(), *child_runs.get()), (2, 2));
}

#[lithia::test]
async fn status_follows_runs_failures_and_writes() {
    let n: SourceNode<u32> = effect!(1);
    let derived: DerivedNode<u32> = effect!(|n| *n.get());
    assert!(matches!(lithia::status(&derived), Status::Pending));
    assert!(matches!(lithia::status(&n), Status::Ready));
    lithia::flush().await;
    assert!(matches!(lithia::status(&derived), Status::Ready));

    let broken: SourceNode<u32> = effect!(async {
        tokio::time::sleep(Duration::from_millis(5)).await;
        panic!("async source failed")
    });
    lithia::test::settled(&broken).await;
    match lithia::status(&broken) {
        Status::Failed(failure) => assert_eq!(failure.message, "async source failed"),
        other => panic!("expected Failed, got {other:?}"),
    }
    broken.set(4); // a write is a new run, which clears the failure
    assert!(matches!(lithia::status(&broken), Status::Ready));
    assert_eq!(*broken.get(), 4);
}

#[lithia::test]
async fn a_write_before_the_first_item_keeps_the_block_from_running() {
    let ran: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let flag: Arc<AtomicBool> = ran.clone();
    let slow: SourceNode<u32> = effect!(async move {
        flag.store(true, Ordering::SeqCst);
        1
    });
    slow.set(2);
    lithia::test::settled(&slow).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!ran.load(Ordering::SeqCst), "a superseded block never starts");
    assert_eq!(*slow.get(), 2);
}
