// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Validation cases BL–BW: one rule in every context (spec §13).
//! Every case runs under the workspace's test timeout (`.cargo/config.toml`).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lithia::{Accessor, DerivedNode, SourceNode, Status, node};
use tokio::sync::oneshot;

// Case BL: every body form sees its captures by reference; a `move` block owns what it moved in.
#[lithia::test]
async fn case_bl_captures_by_reference() {
    let n: SourceNode<u32> = node!(1);
    let offset: u32 = 10;
    let sync_body: DerivedNode<u32> = node!(|n| *n.get() + *offset); // `offset` is a `&u32` here
    let async_body: DerivedNode<u32> = node!(async |n| *n.get() + *offset); // and here
    let (tx, rx): (oneshot::Sender<u32>, oneshot::Receiver<u32>) = oneshot::channel();
    let _block: SourceNode<()> = node!(async move {
        let _ = tx.send(offset); // runs once, so it owns what it moved in and can consume it
    });
    lithia::test::settled(&n).await;
    assert_eq!((*sync_body.get(), *async_body.get(), rx.await.unwrap()), (11, 11, 10));
}

// Case BM: an option's value may be any expression of its type, not only one of its constants.
#[lithia::test]
async fn case_bm_computed_option_values() {
    let n: SourceNode<i32> = node!(1);
    let inv: DerivedNode<i32> = node!(|n| 100 / *n.get());
    let policy: lithia::resilience::Resilience = lithia::resilience::NONE;
    let strict: DerivedNode<i32> = node!(|inv| *inv.get(), resilience = policy);
    let lenient: DerivedNode<i32> = node!(|inv| *inv.get(), resilience = if policy == NONE { COMPLETE } else { NONE });
    lithia::flush().await;
    n.set(0); // `inv` fails
    lithia::flush().await;
    assert!(matches!(lithia::status(&strict), Status::Failed(_)));
    assert!(matches!(lithia::status(&lenient), Status::Ready));
}

// Case BN: a reclaim needs the same shape, and takes the creation's comparator.
#[lithia::test]
async fn case_bn_reclaim_shape_and_comparator() {
    let config: SourceNode<(bool, bool)> = node!((false, false)); // (non-resilient, compare by tens)
    let input: SourceNode<u32> = node!(0);
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let made: DerivedNode<Option<DerivedNode<u32>>> = node!(|config| {
        let (strict, by_tens): (bool, bool) = *config.get();
        Some(node!(
            |input| {
                runs.fetch_add(1, Ordering::SeqCst);
                *input.get()
            },
            key = 0u8,
            resilience = if strict { NONE } else { PARTIAL },
            equals = move |a: &u32, b: &u32| by_tens && a / 10 == b / 10
        ))
    });
    lithia::flush().await;
    let first: DerivedNode<u32> = (*made.get()).clone().unwrap();
    config.set((false, true)); // the same shape: reclaimed without a run, with the new comparator
    lithia::flush().await;
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    input.set(5); // equal by tens, so the node keeps 0
    lithia::flush().await;
    assert_eq!((*first.get(), runs.load(Ordering::SeqCst)), (0, 2));
    config.set((true, true)); // another resilience: a new node takes the key, and runs
    lithia::flush().await;
    assert_eq!(runs.load(Ordering::SeqCst), 3);
}

// Case BO: a new child of a ghost behaves as a surviving child; with only ghosts for parents, it is one.
#[lithia::test]
async fn case_bo_new_children_of_ghosts() {
    let frozen: SourceNode<i32> = node!(5, disposed = MASK);
    let doomed: SourceNode<i32> = node!(7); // `disposed = FAIL`, the default
    let k: SourceNode<i32> = node!(2);
    lithia::dispose(&frozen);
    lithia::dispose(&doomed);
    let late: DerivedNode<i32> = node!(|frozen, k| *frozen.get() * *k.get());
    let strict: DerivedNode<i32> = node!(|doomed, k| *k.get(), resilience = NONE);
    let orphan: DerivedNode<i32> = node!(|frozen| *frozen.get());
    lithia::flush().await;
    assert_eq!(*late.get(), 10); // the ghost's last value, frozen
    match lithia::status(&strict) {
        Status::Failed(failure) => assert_eq!(failure.causes[0].message, "parent disposed"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(matches!(lithia::status(&orphan), Status::Ghost)); // as disposal would have made it
}

// Case BP: a handle a `move` block moves out counts as an outside handle once the block has ended.
#[lithia::test]
async fn case_bp_a_handle_moved_out_of_a_block() {
    let seen: SourceNode<u32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    let (tx, rx): (oneshot::Sender<SourceNode<u32>>, oneshot::Receiver<SourceNode<u32>>) = oneshot::channel();
    let t: SourceNode<u32> = node!(3);
    let block: SourceNode<()> = node!(async move {
        let _ = tx.send(t);
    });
    let moved: SourceNode<u32> = rx.await.unwrap();
    let watcher: DerivedNode<()> = node!(|moved| seen.set(*moved.get()));
    lithia::flush().await;
    drop((block, watcher)); // `moved` anchors `t`, so support keeps the watcher
    assert_eq!(lithia::test::live_nodes().await, base + 2);
    moved.set(4);
    lithia::flush().await;
    assert_eq!(*seen.get(), 4);
    drop(moved);
    assert_eq!(lithia::test::live_nodes().await, base);
}

// Case BQ: an anchor's return holds again what support let go of while something kept it alive.
#[lithia::test]
async fn case_bq_an_anchor_returns() {
    let go: SourceNode<u32> = node!(0);
    let slot: Arc<Mutex<Option<Accessor<u32>>>> = Arc::new(Mutex::new(None));
    let base: usize = lithia::test::live_nodes().await;
    let p: SourceNode<u32> = node!(1);
    let d: DerivedNode<u32> = node!(|p| *p.get());
    let e: DerivedNode<()> = node!(|d| {
        let _ = d.get();
    });
    // `w`'s body keeps `d` and `e` alive without anchoring them, and can hand out `d` again.
    let w: DerivedNode<()> = node!(|go| {
        if *go.get() > 0 {
            *slot.lock().unwrap() = Some(d.accessor());
        }
        let _ = e.get();
    })
    .scoped();
    lithia::flush().await;
    drop((e, d, p)); // support lets go of `d` and `e`
    go.set(1); // `d` is an anchor again: support holds `d` and `e` again
    lithia::flush().await;
    drop(w);
    assert_eq!(lithia::test::live_nodes().await, base + 3);
    slot.lock().unwrap().take();
    assert_eq!(lithia::test::live_nodes().await, base);
}

// Case BR: a creator decides its children before they run: a keyed child runs only with its captures.
#[lithia::test]
async fn case_br_creator_order() {
    let input: SourceNode<u32> = node!(0);
    let seen: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
    let _creator: DerivedNode<()> = node!(|input| {
        let label: u32 = *input.get();
        node!(|input| seen.lock().unwrap().push((*label, *input.get())), key = 0u8);
    });
    lithia::flush().await;
    for value in 1..=3u32 {
        input.set(value);
        lithia::flush().await;
    }
    // Never the old label with the new input, and one run per change.
    assert_eq!(*seen.lock().unwrap(), [(0, 0), (1, 1), (2, 2), (3, 3)]);
}

// Case BS: an async node whose input outpaces its body still publishes.
#[lithia::test]
async fn case_bs_progress_under_fast_input() {
    let input: SourceNode<u32> = node!(0);
    let slow: DerivedNode<u32> = node!(async |input| {
        let value: u32 = *input.get();
        tokio::time::sleep(Duration::from_millis(8)).await;
        value
    });
    let mut published: Vec<u32> = Vec::new();
    for value in 1..=40u32 {
        input.set(value);
        lithia::flush().await;
        tokio::time::sleep(Duration::from_millis(2)).await;
        published.push(*slow.get());
    }
    published.dedup();
    assert!(published.len() > 2, "{published:?}"); // a replacement run finishes before it is replaced
    lithia::test::settled(&slow).await;
    assert_eq!(*slow.get(), 40);
}

// Case BT: a non-resilient child whose parent recovers unchanged is restored without running.
#[lithia::test]
async fn case_bt_resilience_recovers_by_status() {
    let n: SourceNode<i32> = node!(1);
    let inv: DerivedNode<i32> = node!(|n| 100 / *n.get(), equals = lithia::eq);
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let strict: DerivedNode<i32> = node!(
        |inv| {
            runs.fetch_add(1, Ordering::SeqCst);
            *inv.get() + 1
        },
        resilience = NONE
    );
    lithia::flush().await;
    n.set(0); // `inv` fails, and `strict` with it, without running
    lithia::flush().await;
    assert!(matches!(lithia::status(&strict), Status::Failed(_)));
    n.set(1); // `inv` recovers to an equal value: only its status changed
    lithia::flush().await;
    assert!(matches!(lithia::status(&strict), Status::Ready));
    assert_eq!((*strict.get(), runs.load(Ordering::SeqCst)), (101, 1));
}

// Case BU: a cut writer reruns once its epoch has settled, leaving alone what it wrote.
#[lithia::test]
async fn case_bu_a_cut_writer_reruns() {
    let x: SourceNode<i32> = node!(0);
    let doubled: DerivedNode<i32> = node!(|x| *x.get() * 2);
    let over: DerivedNode<bool> = node!(|doubled| {
        let over: bool = *doubled.get() > 20;
        if over {
            x.set(10);
        }
        over
    });
    x.set(15);
    lithia::flush().await;
    // Cut on its own write, it reran on the settled 20, and is no longer over.
    assert_eq!((*x.get(), *doubled.get(), *over.get()), (10, 20, false));
}

async fn reads_of_a_join(batches: lithia::batches::Batches) -> Vec<(u32, u32)> {
    let a: SourceNode<u32> = node!(0);
    let b: SourceNode<u32> = node!(0);
    // `slow` holds `b`'s epoch at the gate until `a`'s epoch has reached the join.
    let (open, gate): (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>) = std::sync::mpsc::channel();
    let gate: Arc<Mutex<std::sync::mpsc::Receiver<()>>> = Arc::new(Mutex::new(gate));
    let slow: DerivedNode<u32> = node!(|b| {
        let value: u32 = *b.get();
        if value > 0 {
            // Blocking hands the worker's queue on, or a run tokio queued behind this one waits too.
            tokio::task::block_in_place(|| {
                let _ = gate.lock().unwrap().recv();
            });
        }
        value * 10
    });
    let seen: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
    let _join: DerivedNode<()> = node!(
        |a, slow| {
            let pair: (u32, u32) = (*a.get(), *slow.get());
            seen.lock().unwrap().push(pair);
        },
        batches = batches
    );
    lithia::flush().await;
    b.set(1);
    tokio::task::yield_now().await; // delivers `b`'s epoch, which frames the join and waits in `slow`
    a.set(1);
    tokio::task::yield_now().await;
    if batches == lithia::batches::PARTIAL {
        // `a`'s epoch runs the join while `b`'s is still held. Bounded below the harness timeout, so
        // a stall says which step it was, and that timers still ran.
        let deadline: tokio::time::Instant = tokio::time::Instant::now() + Duration::from_secs(10);
        while seen.lock().unwrap().len() < 2 {
            assert!(tokio::time::Instant::now() < deadline, "the join never ran on `a`'s epoch: {seen:?}");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    open.send(()).unwrap();
    let settled: Result<(), tokio::time::error::Elapsed> =
        tokio::time::timeout(Duration::from_secs(10), lithia::flush()).await;
    assert!(settled.is_ok(), "`b`'s epoch never settled once `slow` was let go: {seen:?}");
    let reads: Vec<(u32, u32)> = seen.lock().unwrap().clone();
    reads
}

// Case BV: a `batches = WHOLE` join waits for every batch that has reached it.
// Two workers, so `a` is written while `slow` blocks one of them.
#[lithia::test(flavor = "multi_thread", worker_threads = 2)]
async fn case_bv_whole_batches() {
    // On partial batches, `a`'s runs the join while `b`'s is still on its way through `slow`.
    assert_eq!(reads_of_a_join(lithia::batches::PARTIAL).await, [(0, 0), (1, 0), (1, 10)]);
    assert_eq!(reads_of_a_join(lithia::batches::WHOLE).await, [(0, 0), (1, 10)]);
}

// Case BW: `lithia::test::settled` follows the subtree's writes.
#[lithia::test]
async fn case_bw_settled_follows_writes() {
    let input: SourceNode<u32> = node!(0);
    let mirror: SourceNode<u32> = node!(0);
    let _writer: DerivedNode<()> = node!(|input| mirror.set(*input.get()));
    let slow: DerivedNode<u32> = node!(async |mirror| {
        let value: u32 = *mirror.get();
        tokio::time::sleep(Duration::from_millis(30)).await;
        value
    });
    lithia::test::settled(&slow).await;
    input.set(5);
    lithia::test::settled(&input).await; // `slow` is downstream of a node the writer writes
    assert_eq!(*slow.get(), 5);
}
