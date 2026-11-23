// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Runtimes, flavours and shutdown (spec §11.3).

use std::time::Duration;

use lithia::{DerivedNode, SourceNode, Status, effect};

#[test]
fn a_run_whose_runtime_shut_down_fails_its_node() {
    let runtime: tokio::runtime::Runtime = tokio::runtime::Runtime::new().unwrap();
    let (n, sleepy): (SourceNode<u32>, DerivedNode<u32>) = runtime.block_on(async {
        let n: SourceNode<u32> = effect!(1);
        let sleepy: DerivedNode<u32> = effect!(async |n| {
            tokio::time::sleep(Duration::from_secs(60)).await;
            *n.get()
        });
        tokio::time::sleep(Duration::from_millis(20)).await; // let the run start and suspend
        (n, sleepy)
    });
    drop(runtime);
    match lithia::status(&sleepy) {
        Status::Failed(failure) => assert_eq!(failure.message, "runtime shut down"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(*n.get(), 1);
}

#[test]
fn a_derived_node_needs_a_runtime() {
    let n: SourceNode<u32> = effect!(1); // source nodes need none
    let created: std::thread::Result<DerivedNode<u32>> =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| effect!(|n| *n.get())));
    assert!(created.is_err(), "as tokio::spawn would, creation without a runtime panics");
}

#[lithia::test(flavor = "current_thread")]
async fn the_current_thread_flavour_works() {
    let a: SourceNode<u32> = effect!(2);
    let squared: DerivedNode<u32> = effect!(|a| *a.get() * *a.get());
    lithia::flush().await;
    a.set(3);
    lithia::flush().await;
    assert_eq!(*squared.get(), 9);
}

#[lithia::test(flavor = "multi_thread", worker_threads = 1)]
async fn spawn_blocking_outside_bodies_delivers_in_the_caller_scope() {
    let a: SourceNode<u32> = effect!(0);
    let doubled: DerivedNode<u32> = effect!(|a| *a.get() * 2);
    let writer: SourceNode<u32> = a.clone();
    lithia::spawn_blocking(move || writer.set(21)).await.unwrap();
    lithia::flush().await;
    assert_eq!(*doubled.get(), 42);
}

#[lithia::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wide_fan_out_runs_every_child_once_per_write() {
    // More ready children than the workers' runner tasks hold, so they are packed (spec §11.4, step 4).
    const WIDTH: usize = 1000;
    let runs: std::sync::Arc<std::sync::atomic::AtomicUsize> = Default::default();
    let hub: SourceNode<u64> = effect!(0);
    let children: Vec<DerivedNode<u64>> = (0..WIDTH)
        .map(|_| {
            let runs: std::sync::Arc<std::sync::atomic::AtomicUsize> = runs.clone();
            effect!(|hub| {
                runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                *hub.get() * 2
            })
        })
        .collect();
    lithia::flush().await;
    for round in 1..=3u64 {
        hub.set(round);
        lithia::flush().await;
        assert!(children.iter().all(|child: &DerivedNode<u64>| *child.get() == round * 2));
    }
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 4 * WIDTH, "first runs and three writes");
}

#[lithia::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chain_longer_than_one_task_runs_in_order() {
    // Past one task's budget the chain goes on in another task, still in order.
    let source: SourceNode<u64> = effect!(0);
    let mut tail: DerivedNode<u64> = effect!(|source| *source.get() + 1);
    let mut chain: Vec<DerivedNode<u64>> = Vec::new();
    for _ in 1..300 {
        let previous: DerivedNode<u64> = tail.clone();
        chain.push(previous.clone());
        tail = effect!(|previous| *previous.get() + 1);
    }
    lithia::flush().await;
    for round in [10u64, 1000, 7] {
        source.set(round);
        lithia::flush().await;
        assert_eq!(*tail.get(), round + 300);
    }
}

#[lithia::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panic_on_the_fast_path_fails_only_its_node() {
    let a: SourceNode<u32> = effect!(1);
    let p: DerivedNode<u32> = effect!(|a| {
        assert!(*a.get() != 2, "two is refused");
        *a.get() * 10
    });
    let q: DerivedNode<u32> = effect!(|p| *p.get() + 1);
    let s: DerivedNode<u32> = effect!(|a| *a.get() + 100);
    let t: DerivedNode<u32> = effect!(|s| *s.get() * 2);
    lithia::flush().await;
    a.set(2);
    lithia::flush().await;
    assert!(matches!(lithia::status(&p), Status::Failed(_)));
    assert_eq!((*s.get(), *t.get()), (102, 204), "runs after the panic still ran");
    a.set(3);
    lithia::flush().await;
    assert_eq!((*p.get(), *q.get(), *s.get(), *t.get()), (30, 31, 103, 206));
}

#[test]
fn bodies_run_on_their_own_runtime_on_the_fast_path_too() {
    type Seen = std::sync::Arc<std::sync::Mutex<Vec<(bool, tokio::runtime::Id)>>>;
    /// A child of `parent` made on `runtime`, noting where each of its runs ran.
    fn record(
        runtime: &tokio::runtime::Runtime,
        on_a: bool,
        parent: &DerivedNode<u32>,
        seen: &Seen,
    ) -> DerivedNode<u32> {
        let seen: Seen = seen.clone();
        let parent: DerivedNode<u32> = parent.clone();
        runtime.block_on(async {
            effect!(|parent| {
                seen.lock().unwrap().push((*on_a, tokio::runtime::Handle::current().id()));
                *parent.get()
            })
        })
    }
    let rt_a: tokio::runtime::Runtime =
        tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let rt_b: tokio::runtime::Runtime =
        tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let (a_id, b_id): (tokio::runtime::Id, tokio::runtime::Id) = (rt_a.handle().id(), rt_b.handle().id());
    let seen: Seen = Default::default();
    let hub: SourceNode<u32> = rt_a.block_on(async { effect!(0) });
    let top: DerivedNode<u32> = rt_a.block_on(async { effect!(|hub| *hub.get()) });
    // Children of one node, half on each runtime: a run on one runtime readies runs of both.
    let children: Vec<DerivedNode<u32>> = (0..40)
        .map(
            |i: usize| {
                if i.is_multiple_of(2) { record(&rt_a, true, &top, &seen) } else { record(&rt_b, false, &top, &seen) }
            },
        )
        .collect();
    // And a chain crossing over, one run at a time.
    let middle: DerivedNode<u32> = rt_a.block_on(async { effect!(|hub| *hub.get() + 1) });
    let across: DerivedNode<u32> = record(&rt_b, false, &middle, &seen);
    rt_a.block_on(async { lithia::flush().await });
    rt_b.block_on(async { lithia::flush().await });
    rt_a.block_on(async {
        hub.set(5);
        lithia::flush().await;
    });
    rt_b.block_on(async { lithia::flush().await });
    assert!(children.iter().all(|child: &DerivedNode<u32>| *child.get() == 5));
    assert_eq!(*across.get(), 6);
    let seen: Vec<(bool, tokio::runtime::Id)> = seen.lock().unwrap().clone();
    assert!(seen.len() >= 40, "every child ran for the write");
    assert!(seen.iter().all(|(on_a, id): &(bool, tokio::runtime::Id)| *id == if *on_a { a_id } else { b_id }));
}

#[test]
fn ready_runs_packed_for_a_runtime_that_shuts_down_fail_like_runs() {
    // A runtime with no thread of its own polls nothing outside `block_on`, so the runner tasks
    // a write packs stay queued until it shuts down.
    let runtime: tokio::runtime::Runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let (hub, children): (SourceNode<u32>, Vec<DerivedNode<u32>>) = runtime.block_on(async {
        let hub: SourceNode<u32> = effect!(1);
        let children: Vec<DerivedNode<u32>> = (0..40).map(|_| effect!(|hub| *hub.get())).collect();
        lithia::flush().await;
        (hub, children)
    });
    hub.set(2); // from outside the runtime: its runs are queued there
    drop(runtime);
    for child in &children {
        match lithia::status(child) {
            Status::Failed(failure) => assert_eq!(failure.message, "runtime shut down"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}

#[lithia::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_body_blocking_on_flush_does_not_wait_for_its_own_poll_to_end() {
    let input: SourceNode<u32> = effect!(0);
    let out: SourceNode<u32> = effect!(0);
    let doubled: DerivedNode<u32> = effect!(|out| *out.get() * 2);
    let _blocker: DerivedNode<()> = effect!(|input| {
        let value: u32 = *input.get();
        if value > 0 {
            out.set(value);
            // Waits for `doubled`, which the write readied while this body polls.
            tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(lithia::flush()));
        }
    });
    lithia::flush().await;
    input.set(5);
    let flushed: Result<(), tokio::time::error::Elapsed> =
        tokio::time::timeout(Duration::from_secs(5), lithia::flush()).await;
    assert!(flushed.is_ok(), "the body's flush waited for a run queued behind its own poll");
    assert_eq!(*doubled.get(), 10);
}
