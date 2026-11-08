//! Propagation, batching, scopes and termination (spec §6.2, §7, §11.3).

use std::time::Duration;

use lithia::{Accessor, DerivedNode, SourceNode, effect};

#[lithia::test]
async fn a_wide_join_runs_once_per_change() {
    let a: SourceNode<i64> = effect!(1);
    let runs: SourceNode<u32> = effect!(0);
    let b1: DerivedNode<i64> = effect!(|a| *a.get() + 1);
    let b2: DerivedNode<i64> = effect!(|a| *a.get() + 2);
    let b3: DerivedNode<i64> = effect!(|a| *a.get() + 3);
    let b4: DerivedNode<i64> = effect!(|a| *a.get() + 4);
    let c1: DerivedNode<i64> = effect!(|b1, b2| *b1.get() + *b2.get());
    let c2: DerivedNode<i64> = effect!(|b3, b4| *b3.get() + *b4.get());
    let join: DerivedNode<i64> = effect!(|c1, c2, a| {
        runs.update(|n: &u32| n + 1);
        *c1.get() + *c2.get() + *a.get()
    });
    lithia::flush().await;
    assert_eq!((*join.get(), *runs.get()), (15, 1));
    for value in 2..=20i64 {
        a.set(value);
        lithia::flush().await;
        assert_eq!(*join.get(), 5 * value + 10, "consistent inputs only");
    }
    assert_eq!(*runs.get(), 20, "one run per change, never a glitch");
}

#[lithia::test]
async fn one_node_through_two_handles_is_one_dependency() {
    let a: SourceNode<u32> = effect!(1);
    let view: Accessor<u32> = a.accessor();
    let runs: SourceNode<u32> = effect!(0);
    let both: DerivedNode<u32> = effect!(|a, view| {
        runs.update(|n: &u32| n + 1);
        *a.get() + *view.get()
    });
    lithia::flush().await;
    a.set(5);
    lithia::flush().await;
    assert_eq!((*both.get(), *runs.get()), (10, 2));
}

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
    let base: usize = lithia::test::live_nodes();
    // Frees 20 001 nodes bottom-up: parents through a worklist, bodies (which hold the
    // captured previous node) on the blocking pool, so neither path recurses.
    drop(tail);
    let freed: Result<(), tokio::time::error::Elapsed> = tokio::time::timeout(Duration::from_secs(15), async {
        while lithia::test::live_nodes() != base - 20_001 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(freed.is_ok(), "live nodes: {} of {}", lithia::test::live_nodes(), base - 20_001);
}

#[lithia::test]
async fn a_four_way_binding_mesh_terminates_and_agrees() {
    let a: SourceNode<i32> = effect!(0, equals = lithia::eq);
    let b: SourceNode<i32> = effect!(0, equals = lithia::eq);
    let c: SourceNode<i32> = effect!(0, equals = lithia::eq);
    let d: SourceNode<i32> = effect!(0, equals = lithia::eq);
    let _ab: DerivedNode<()> = effect!(|a| {
        b.set(*a.get());
        c.set(*a.get());
        d.set(*a.get());
    });
    let _bc: DerivedNode<()> = effect!(|b| {
        a.set(*b.get());
        c.set(*b.get());
        d.set(*b.get());
    });
    let _cd: DerivedNode<()> = effect!(|c| {
        a.set(*c.get());
        b.set(*c.get());
        d.set(*c.get());
    });
    let _da: DerivedNode<()> = effect!(|d| {
        a.set(*d.get());
        b.set(*d.get());
        c.set(*d.get());
    });
    lithia::flush().await;
    a.set(5);
    lithia::flush().await;
    assert_eq!((*a.get(), *b.get(), *c.get(), *d.get()), (5, 5, 5, 5));
    c.set(7);
    lithia::flush().await;
    assert_eq!((*a.get(), *b.get(), *c.get(), *d.get()), (7, 7, 7, 7));
}

#[lithia::test]
async fn an_unconditional_mesh_still_terminates() {
    let a: SourceNode<u64> = effect!(0);
    let b: SourceNode<u64> = effect!(0);
    let c: SourceNode<u64> = effect!(0);
    let _ab: DerivedNode<()> = effect!(|a| b.set(*a.get() + 1));
    let _bc: DerivedNode<()> = effect!(|b| c.set(*b.get() + 1));
    let _ca: DerivedNode<()> = effect!(|c| a.set(*c.get() + 1));
    let done: Result<(), tokio::time::error::Elapsed> = tokio::time::timeout(Duration::from_secs(5), async {
        lithia::flush().await;
        a.set(10);
        lithia::flush().await;
    })
    .await;
    assert!(done.is_ok(), "every writer appears at most once per chain, so the loop is cut");
}

#[lithia::test]
async fn flush_inside_a_body_waits_for_what_it_caused() {
    let target: SourceNode<u32> = effect!(0);
    let doubled: DerivedNode<u32> = effect!(|target| *target.get() * 2);
    let trigger: SourceNode<u32> = effect!(0);
    let seen: SourceNode<u32> = effect!(0);
    let _writer: DerivedNode<()> = effect!(async |trigger| {
        let value: u32 = *trigger.get();
        if value > 0 {
            target.set(value);
            lithia::flush().await;
            seen.set(*doubled.get());
        }
    });
    lithia::flush().await;
    trigger.set(21);
    lithia::test::settled(&trigger).await;
    assert_eq!(*seen.get(), 42);
}

#[lithia::test]
async fn a_lithia_task_batches_each_poll() {
    let a: SourceNode<u32> = effect!(0);
    let b: SourceNode<u32> = effect!(0);
    let runs: SourceNode<u32> = effect!(0);
    let _sum: DerivedNode<u32> = effect!(|a, b| {
        runs.update(|n: &u32| n + 1);
        *a.get() + *b.get()
    });
    lithia::flush().await;
    let (a2, b2): (SourceNode<u32>, SourceNode<u32>) = (a.clone(), b.clone());
    lithia::spawn(async move {
        a2.set(1);
        b2.set(2); // same poll, same batch: the join runs once
        lithia::flush().await;
    })
    .await
    .unwrap();
    assert_eq!(*runs.get(), 2);
}

#[lithia::test]
async fn a_plain_tokio_task_writes_one_batch_per_write() {
    let a: SourceNode<u32> = effect!(0);
    let b: SourceNode<u32> = effect!(0);
    let sum: DerivedNode<u32> = effect!(|a, b| *a.get() + *b.get());
    lithia::flush().await;
    let (a2, b2): (SourceNode<u32>, SourceNode<u32>) = (a.clone(), b.clone());
    tokio::spawn(async move {
        a2.set(1);
        b2.set(2);
        lithia::flush().await; // waits for this task's own epochs
    })
    .await
    .unwrap();
    assert_eq!(*sum.get(), 3);
}

#[lithia::test]
async fn defer_runs_after_the_epoch_and_flush_waits_for_it() {
    let x: SourceNode<u32> = effect!(1);
    let doubled: DerivedNode<u32> = effect!(|x| *x.get() * 2);
    let seen: SourceNode<u32> = effect!(0);
    lithia::flush().await;
    x.set(5);
    let (doubled2, seen2): (DerivedNode<u32>, SourceNode<u32>) = (doubled.clone(), seen.clone());
    lithia::defer(move || seen2.set(*doubled2.get()));
    lithia::flush().await;
    assert_eq!(*seen.get(), 10, "the deferred function saw the settled epoch");
}

#[test]
fn defer_outside_lithia_tasks_follows_the_latest_write() {
    let runtime: tokio::runtime::Runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let x: SourceNode<u32> = effect!(1);
        let doubled: DerivedNode<u32> = effect!(|x| *x.get() * 2);
        let seen: SourceNode<u32> = effect!(0);
        x.set(4);
        let (doubled2, seen2): (DerivedNode<u32>, SourceNode<u32>) = (doubled.clone(), seen.clone());
        lithia::defer(move || seen2.set(*doubled2.get()));
        lithia::flush().await;
        assert_eq!(*seen.get(), 8);
    });
}

#[lithia::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writers_reach_eventual_consistency() {
    let sources: Vec<SourceNode<u64>> = (0..8).map(|_| effect!(0u64)).collect();
    let (s0, s1, s2, s3, s4, s5, s6, s7): (
        SourceNode<u64>,
        SourceNode<u64>,
        SourceNode<u64>,
        SourceNode<u64>,
        SourceNode<u64>,
        SourceNode<u64>,
        SourceNode<u64>,
        SourceNode<u64>,
    ) = (
        sources[0].clone(),
        sources[1].clone(),
        sources[2].clone(),
        sources[3].clone(),
        sources[4].clone(),
        sources[5].clone(),
        sources[6].clone(),
        sources[7].clone(),
    );
    let left: DerivedNode<u64> = effect!(|s0, s1, s2, s3| *s0.get() + *s1.get() + *s2.get() + *s3.get());
    let right: DerivedNode<u64> = effect!(|s4, s5, s6, s7| *s4.get() + *s5.get() + *s6.get() + *s7.get());
    let total: DerivedNode<u64> = effect!(|left, right| *left.get() + *right.get());
    lithia::flush().await;
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        let source: SourceNode<u64> = source.clone();
        tasks.push(lithia::spawn(async move {
            for step in 1..=200u64 {
                source.set(step * (index as u64 + 1));
                if step % 7 == 0 {
                    tokio::task::yield_now().await;
                }
            }
            lithia::flush().await;
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    lithia::test::settled(&s0).await;
    for source in &sources {
        lithia::test::settled(source).await;
    }
    let expected: u64 = (1..=8u64).map(|k| 200 * k).sum();
    assert_eq!(*total.get(), expected, "once writes stop, every value matches its inputs");
}
