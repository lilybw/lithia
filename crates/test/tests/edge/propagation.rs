// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Propagation, batching, scopes and termination (spec §6.2, §7, §11.3).

#![allow(clippy::type_complexity)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use lithia::{Accessor, DerivedNode, SourceNode, node};
use tracing::Level;

#[lithia::test]
async fn a_wide_join_runs_once_per_change() {
    let a: SourceNode<i64> = node!(1);
    let runs: SourceNode<u32> = node!(0);
    let b1: DerivedNode<i64> = node!(|a| *a.get() + 1);
    let b2: DerivedNode<i64> = node!(|a| *a.get() + 2);
    let b3: DerivedNode<i64> = node!(|a| *a.get() + 3);
    let b4: DerivedNode<i64> = node!(|a| *a.get() + 4);
    let c1: DerivedNode<i64> = node!(|b1, b2| *b1.get() + *b2.get());
    let c2: DerivedNode<i64> = node!(|b3, b4| *b3.get() + *b4.get());
    let join: DerivedNode<i64> = node!(|c1, c2, a| {
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
    let a: SourceNode<u32> = node!(1);
    let view: Accessor<u32> = a.accessor();
    let runs: SourceNode<u32> = node!(0);
    let both: DerivedNode<u32> = node!(|a, view| {
        runs.update(|n: &u32| n + 1);
        *a.get() + *view.get()
    });
    lithia::flush().await;
    a.set(5);
    lithia::flush().await;
    assert_eq!((*both.get(), *runs.get()), (10, 2));
}

#[lithia::test]
async fn a_four_way_binding_mesh_terminates_and_agrees() {
    let a: SourceNode<i32> = node!(0, equals = lithia::eq);
    let b: SourceNode<i32> = node!(0, equals = lithia::eq);
    let c: SourceNode<i32> = node!(0, equals = lithia::eq);
    let d: SourceNode<i32> = node!(0, equals = lithia::eq);
    let _ab: DerivedNode<()> = node!(|a| {
        b.set(*a.get());
        c.set(*a.get());
        d.set(*a.get());
    });
    let _bc: DerivedNode<()> = node!(|b| {
        a.set(*b.get());
        c.set(*b.get());
        d.set(*b.get());
    });
    let _cd: DerivedNode<()> = node!(|c| {
        a.set(*c.get());
        b.set(*c.get());
        d.set(*c.get());
    });
    let _da: DerivedNode<()> = node!(|d| {
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
    let a: SourceNode<u64> = node!(0);
    let b: SourceNode<u64> = node!(0);
    let c: SourceNode<u64> = node!(0);
    let _ab: DerivedNode<()> = node!(|a| b.set(*a.get() + 1));
    let _bc: DerivedNode<()> = node!(|b| c.set(*b.get() + 1));
    let _ca: DerivedNode<()> = node!(|c| a.set(*c.get() + 1));
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
    let target: SourceNode<u32> = node!(0);
    let doubled: DerivedNode<u32> = node!(|target| *target.get() * 2);
    let trigger: SourceNode<u32> = node!(0);
    let seen: SourceNode<u32> = node!(0);
    let _writer: DerivedNode<()> = node!(async |trigger| {
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
    let a: SourceNode<u32> = node!(0);
    let b: SourceNode<u32> = node!(0);
    let runs: SourceNode<u32> = node!(0);
    let _sum: DerivedNode<u32> = node!(|a, b| {
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
    let a: SourceNode<u32> = node!(0);
    let b: SourceNode<u32> = node!(0);
    let sum: DerivedNode<u32> = node!(|a, b| *a.get() + *b.get());
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
    let x: SourceNode<u32> = node!(1);
    let doubled: DerivedNode<u32> = node!(|x| *x.get() * 2);
    let seen: SourceNode<u32> = node!(0);
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
        let x: SourceNode<u32> = node!(1);
        let doubled: DerivedNode<u32> = node!(|x| *x.get() * 2);
        let seen: SourceNode<u32> = node!(0);
        x.set(4);
        let (doubled2, seen2): (DerivedNode<u32>, SourceNode<u32>) = (doubled.clone(), seen.clone());
        lithia::defer(move || seen2.set(*doubled2.get()));
        lithia::flush().await;
        assert_eq!(*seen.get(), 8);
    });
}

#[test]
fn defer_outside_lithia_tasks_waits_for_a_slow_run_of_the_latest_write() {
    let runtime: tokio::runtime::Runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let x: SourceNode<u32> = node!(1);
        let doubled: DerivedNode<u32> = node!(|x| {
            let value: u32 = *x.get();
            if value == 4 {
                std::thread::sleep(Duration::from_millis(50));
            }
            value * 2
        });
        let seen: SourceNode<u32> = node!(0);
        lithia::flush().await;
        x.set(4);
        let (doubled2, seen2): (DerivedNode<u32>, SourceNode<u32>) = (doubled.clone(), seen.clone());
        lithia::defer(move || seen2.set(*doubled2.get())); // after the write's epoch, not at once
        lithia::flush().await;
        assert_eq!(*seen.get(), 8);
    });
}

#[lithia::test]
async fn flush_returns_after_a_deferred_function_panics() {
    let _ = lithia_test::events();
    let written: SourceNode<bool> = node!(false);
    let writer: SourceNode<bool> = written.clone();
    lithia::defer(move || {
        writer.set(true);
        panic!("deferred function panics");
    });
    let done: Result<(), tokio::time::error::Elapsed> =
        tokio::time::timeout(Duration::from_secs(2), lithia::flush()).await;
    assert!(done.is_ok(), "flush hung after a deferred function panicked");
    assert!(*written.get(), "a write made before the panic lands");
    let reported: Option<lithia_test::Captured> = lithia_test::find(
        tracing::Level::WARN,
        &["a deferred function panicked", "deferred function panics"],
        Duration::from_secs(2),
    )
    .await;
    assert!(reported.is_some(), "the panic is reported with its message");
}

#[lithia::test(flavor = "multi_thread", worker_threads = 4)]
async fn flush_waits_for_the_run_that_absorbed_its_write() {
    let x: SourceNode<u32> = node!(1);
    let y: SourceNode<u32> = node!(0);
    let started: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let doubled: DerivedNode<u32> = node!(|x, y| {
        let _ = y.get();
        let value: u32 = *x.get() * 2;
        started.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        value
    });
    lithia::flush().await;
    started.store(false, Ordering::SeqCst);
    // `x`'s batch waits for this poll to end; meanwhile a write from another thread starts a run,
    // which reads `x` = 4 before `x`'s epoch reaches the node.
    x.set(4);
    let y2: SourceNode<u32> = y.clone();
    std::thread::spawn(move || y2.set(1));
    while !started.load(Ordering::SeqCst) {
        std::hint::spin_loop();
    }
    lithia::flush().await;
    assert_eq!(*doubled.get(), 8);
}

#[lithia::test(flavor = "multi_thread", worker_threads = 4)]
async fn flush_waits_for_the_successor_of_the_run_that_absorbed_its_write() {
    let x: SourceNode<u32> = node!(1);
    let y: SourceNode<u32> = node!(0);
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    // A slow child of `x` that never changes: `joined` waits for it in `x`'s epoch.
    let slow: DerivedNode<bool> = node!(
        |x| {
            let large: bool = *x.get() > 100;
            tokio::task::block_in_place(|| std::thread::sleep(Duration::from_millis(300)));
            large
        },
        equals = lithia::eq
    );
    let joined: DerivedNode<u32> = node!(
        |x, y, slow| {
            let value: u32 = *x.get() + *y.get() * 10 + u32::from(*slow.get()) * 100;
            runs.fetch_add(1, Ordering::SeqCst);
            tokio::task::block_in_place(|| std::thread::sleep(Duration::from_millis(100)));
            value
        },
        batches = WHOLE
    );
    lithia::flush().await;
    let before: u32 = runs.load(Ordering::SeqCst);
    // `x`'s batch waits for this poll to end. Meanwhile a write from another thread starts a run of
    // `joined`, which reads `x` = 4, and a second write supersedes it; its successor waits for `x`'s
    // epoch, which `slow` holds, and `x`'s entry then finds only the handover the run left.
    x.set(4);
    let (y2, seen): (SourceNode<u32>, Arc<AtomicU32>) = (y.clone(), runs.clone());
    let writer: std::thread::JoinHandle<()> = std::thread::spawn(move || {
        y2.set(1);
        while seen.load(Ordering::SeqCst) == before {
            std::hint::spin_loop();
        }
        y2.set(2);
    });
    while runs.load(Ordering::SeqCst) == before {
        std::hint::spin_loop();
    }
    lithia::flush().await;
    assert_eq!(*joined.get(), 24, "flush returned before the run that includes x = 4");
    writer.join().unwrap();
}

#[lithia::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writers_reach_eventual_consistency() {
    let sources: Vec<SourceNode<u64>> = (0..8).map(|_| node!(0u64)).collect();
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
    let left: DerivedNode<u64> = node!(|s0, s1, s2, s3| *s0.get() + *s1.get() + *s2.get() + *s3.get());
    let right: DerivedNode<u64> = node!(|s4, s5, s6, s7| *s4.get() + *s5.get() + *s6.get() + *s7.get());
    let total: DerivedNode<u64> = node!(|left, right| *left.get() + *right.get());
    lithia::flush().await;
    let mut tasks: Vec<lithia::JoinHandle<()>> = Vec::new();
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
    let expected: u64 = (1..=8u64).map(|k: u64| 200 * k).sum();
    assert_eq!(*total.get(), expected, "once writes stop, every value matches its inputs");
}

#[lithia::test]
async fn siblings_from_one_site_write_as_separate_lineages() {
    let go: SourceNode<u32> = node!(0);
    let shared: SourceNode<u32> = node!(0);
    let out: SourceNode<u32> = node!(0);
    let _creator: DerivedNode<()> = node!(|go| {
        let _ = go.get();
        for first in [true, false] {
            // One site, two places in each run: the second reruns on the first's write.
            node!(|go, shared| {
                if *first {
                    shared.set(*go.get() + 1);
                } else {
                    out.set(*shared.get() * 10);
                }
            });
        }
    });
    lithia::flush().await;
    go.set(5);
    lithia::test::settled(&go).await;
    assert_eq!((*shared.get(), *out.get()), (6, 60));
}

#[lithia::test]
async fn a_loop_through_creation_is_cut_at_the_lineage() {
    let n: SourceNode<u32> = node!(0);
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let _holder: DerivedNode<()> = node!(|n| {
        runs.fetch_add(1, Ordering::SeqCst);
        node!(|n| n.update(|v: &u32| v + 1));
    });
    lithia::flush().await;
    // The first child's write reruns the holder once; the child that run makes is the same
    // lineage, so its first run is cut.
    assert_eq!((*n.get(), runs.load(Ordering::SeqCst)), (1, 2));
}

#[lithia::test]
async fn a_loop_through_two_levels_of_creation_is_cut_at_the_lineage() {
    let n: SourceNode<u32> = node!(0);
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let _holder: DerivedNode<()> = node!(|n| {
        runs.fetch_add(1, Ordering::SeqCst);
        node!(|n| {
            let _ = n.get();
            node!(|n| n.update(|v: &u32| v + 1));
        });
    });
    lithia::flush().await;
    // The rerun's middle node is a new node of the first one's lineage, and so is the child it
    // makes, though the first child was freed with its creator before that.
    assert_eq!((*n.get(), runs.load(Ordering::SeqCst)), (1, 2));
}

#[lithia::test]
async fn a_loop_through_a_keyed_creation_is_cut_when_a_new_node_takes_the_key() {
    let n: SourceNode<u32> = node!(0);
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let _holder: DerivedNode<()> = node!(|n| {
        runs.fetch_add(1, Ordering::SeqCst);
        let strict: bool = (*n.get()).is_multiple_of(2);
        // Another shape on every run, so a new node takes the key, under the key's lineage.
        node!(|n| n.update(|v: &u32| v + 1), key = 7u8, resilience = if strict { NONE } else { PARTIAL });
    });
    lithia::flush().await;
    assert_eq!((*n.get(), runs.load(Ordering::SeqCst)), (1, 2));
}

#[lithia::test]
async fn a_cut_writer_reruns_once_its_epoch_settles() {
    let x: SourceNode<i32> = node!(0, no_equals);
    let doubled: DerivedNode<i32> = node!(|x| *x.get() * 2);
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let over: DerivedNode<bool> = node!(|doubled| {
        runs.fetch_add(1, Ordering::SeqCst);
        let over: bool = *doubled.get() > 20;
        if over {
            x.set(10); // without `equals`, every write propagates: a loop the writer check cuts
        }
        over
    });
    lithia::flush().await;
    x.set(15);
    lithia::flush().await;
    // Cut on its own write, it reran once the epoch settled, saw 20, and wrote nothing.
    assert_eq!((*x.get(), *doubled.get(), *over.get()), (10, 20, false));
    assert_eq!(runs.load(Ordering::SeqCst), 3);
}

#[lithia::test]
async fn a_cut_writer_s_rerun_leaves_alone_what_it_wrote() {
    let x: SourceNode<i32> = node!(0);
    let writes: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let bump: DerivedNode<i32> = node!(|x| {
        let seen: i32 = *x.get();
        if seen > 0 {
            writes.fetch_add(1, Ordering::SeqCst);
            x.set(seen + 1); // always writes: the rerun's write is left alone, so the loop ends
        }
        seen
    });
    lithia::flush().await;
    x.set(1);
    lithia::flush().await;
    assert_eq!((*x.get(), *bump.get()), (2, 2));
    assert_eq!(writes.load(Ordering::SeqCst), 2);
}

async fn reads_of_a_join(batches: lithia::batches::Batches) -> Vec<(u32, u32)> {
    let a: SourceNode<u32> = node!(0);
    let b: SourceNode<u32> = node!(0);
    // `slow` holds `b`'s epoch at the gate until `a`'s epoch has reached the join.
    let (open, gate): (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>) = std::sync::mpsc::channel();
    let gate: Arc<std::sync::Mutex<std::sync::mpsc::Receiver<()>>> = Arc::new(std::sync::Mutex::new(gate));
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
    let seen: Arc<std::sync::Mutex<Vec<(u32, u32)>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
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

#[lithia::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_join_reads_across_batches_unless_it_takes_whole_batches() {
    // On partial batches, `a`'s runs the join while `b`'s is still on its way through `slow`.
    assert_eq!(reads_of_a_join(lithia::batches::PARTIAL).await, [(0, 0), (1, 0), (1, 10)]);
    // On whole batches, the join waits for `b`'s too, and runs once for both.
    assert_eq!(reads_of_a_join(lithia::batches::WHOLE).await, [(0, 0), (1, 10)]);
}

#[lithia::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_node_written_twice_in_one_batch_carries_its_newest_stamp() {
    // A run of `c` starts between the batch's two writes of `s` and reads the first: the batch
    // must carry the second write's stamp, or `c` looks up to date and keeps the stale read.
    let s: SourceNode<u32> = node!(0);
    let t: SourceNode<u32> = node!(0);
    let c: DerivedNode<(u32, u32)> = node!(|s, t| (*s.get(), *t.get()));
    lithia::flush().await;
    let (go, started): (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>) = std::sync::mpsc::channel();
    let other: SourceNode<u32> = t.clone();
    // A plain thread, so its wake-up does not wait in the writing worker's own queue.
    let poke: std::thread::JoinHandle<()> = std::thread::spawn(move || {
        started.recv().unwrap();
        other.set(1); // an outside batch of its own: `c` runs now on another worker, reading `s` = 2
    });
    let writes: lithia::JoinHandle<()> = lithia::spawn(async move {
        s.set(2);
        go.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(100)); // still the same poll, so the same batch
        s.set(3);
    });
    writes.await.unwrap();
    poke.join().unwrap();
    lithia::test::settled(&c).await;
    assert_eq!(*c.get(), (3, 1));
}

#[lithia::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cut_rerun_leaves_alone_what_its_run_recorded_after_the_cut() {
    // Case AA with the race made certain: the blocking write is cut and its epoch settles while the
    // first poll still holds the `runs` write, which it records only when the poll ends. The cut
    // rerun still leaves `runs` alone, since it reads what to leave alone when it starts (7.4).
    let input: SourceNode<u32> = node!(3);
    let result: SourceNode<u32> = node!(0);
    let runs: SourceNode<u32> = node!(0);
    let work: DerivedNode<()> = node!(async |input, result| {
        runs.update(|n: &u32| n + 1);
        let (n, out): (u32, SourceNode<u32>) = (*input.get(), result.clone());
        let blocking: lithia::JoinHandle<()> = lithia::spawn_blocking(move || out.set(n * n));
        std::thread::sleep(Duration::from_millis(100)); // the poll goes on while the write lands
        blocking.await.unwrap();
    });
    lithia::test::settled(&work).await;
    assert_eq!((*result.get(), *runs.get()), (9, 1));
}

#[lithia::test]
async fn settled_follows_every_node_a_run_writes() {
    let input: SourceNode<u32> = node!(0);
    let first: SourceNode<u32> = node!(0);
    let second: SourceNode<u32> = node!(0);
    // Handles the body finds in a value, not captures: its writes through them have no edge.
    let outlets: SourceNode<Vec<SourceNode<u32>>> = node!(vec![first.clone(), second.clone()]);
    let _writer: DerivedNode<()> = node!(|input, outlets| {
        let value: u32 = *input.get();
        for outlet in outlets.get().iter() {
            outlet.set(value);
        }
    });
    let slow: DerivedNode<u32> = node!(async |first| {
        let value: u32 = *first.get();
        tokio::time::sleep(Duration::from_millis(60)).await;
        value
    });
    let quick: DerivedNode<u32> = node!(async |second| {
        let value: u32 = *second.get();
        tokio::time::sleep(Duration::from_millis(1)).await;
        value
    });
    lithia::test::settled(&input).await;
    input.set(5);
    lithia::test::settled(&input).await; // both written nodes, not only the last one noted
    assert_eq!((*slow.get(), *quick.get()), (5, 5));
}

#[lithia::test(flavor = "current_thread")]
async fn tasks_lithia_did_not_start_keep_scopes_of_their_own() {
    let (made, made_rx): (tokio::sync::oneshot::Sender<()>, tokio::sync::oneshot::Receiver<()>) =
        tokio::sync::oneshot::channel();
    let (done, done_rx): (tokio::sync::oneshot::Sender<()>, tokio::sync::oneshot::Receiver<()>) =
        tokio::sync::oneshot::channel();
    let first: tokio::task::JoinHandle<usize> = tokio::spawn(async move {
        let nodes: Vec<SourceNode<u32>> = (0..3).map(|i: u32| node!(i)).collect();
        made.send(()).unwrap();
        let _ = done_rx.await;
        let live: usize = lithia::test::live_nodes().await;
        drop(nodes);
        live
    });
    made_rx.await.unwrap();
    let other: usize = tokio::spawn(async { lithia::test::live_nodes().await }).await.unwrap();
    // More tasks than the scope map keeps before it prunes the scopes that are gone.
    for _ in 0..70 {
        tokio::spawn(async {
            let _ = node!(0u32);
        })
        .await
        .unwrap();
    }
    done.send(()).unwrap();
    assert_eq!(other, 0, "another task's nodes are not in this one's scope");
    assert_eq!(first.await.unwrap(), 3, "a task keeps its scope however many others came and went");
}

#[lithia::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_counts_in_the_scope_of_the_write_it_serves() {
    let x: SourceNode<u32> = node!(0);
    let y: SourceNode<u32> = node!(0);
    let _mirror: DerivedNode<()> = node!(|x| y.set(*x.get()));
    let slow: DerivedNode<u32> = node!(|y| {
        let value: u32 = *y.get();
        if value > 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
        value
    });
    lithia::flush().await;
    let seen: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let (writer, reader, out): (SourceNode<u32>, DerivedNode<u32>, Arc<AtomicU32>) =
        (x.clone(), slow.clone(), seen.clone());
    // The task's write starts runs of nodes made in this test's scope; they count in the task's.
    lithia::spawn(async move {
        writer.set(1);
        lithia::flush().await;
        out.store(*reader.get(), Ordering::SeqCst);
    })
    .await
    .unwrap();
    assert_eq!(seen.load(Ordering::SeqCst), 1, "the task's flush waited for the run its write caused");
}

#[lithia::test]
async fn settled_follows_every_node_a_run_creates() {
    const MADE: u64 = 24;
    let input: SourceNode<u64> = node!(1);
    let creator: DerivedNode<Vec<DerivedNode<u64>>> = node!(|input| {
        let value: u64 = *input.get();
        // More than the trace keeps before it prunes, the first made the slowest.
        (0..MADE)
            .map(|i: u64| {
                node!(
                    async |input| {
                        tokio::time::sleep(Duration::from_millis(4 * (MADE - i))).await;
                        *input.get() * 10 + i
                    },
                    init = value
                )
            })
            .collect::<Vec<DerivedNode<u64>>>()
    });
    lithia::test::settled(&creator).await;
    let values: Vec<u64> = creator.get().iter().map(|child: &DerivedNode<u64>| *child.get()).collect();
    assert_eq!(values, (0..MADE).map(|i: u64| 10 + i).collect::<Vec<u64>>());
}

#[lithia::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_superseded_run_publishes_nothing() {
    let _ = lithia_test::events();
    let n: SourceNode<u32> = node!(0);
    let started: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let release_first: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let first_returned: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let release_second: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let site: String = format!("propagation.rs:{}", line!() + 1);
    let slow: DerivedNode<u32> = node!(|n| {
        let value: u32 = *n.get();
        let gate: Option<&AtomicBool> = match value {
            1 => Some(release_first),
            2 => Some(release_second),
            _ => None,
        };
        if let Some(gate) = gate {
            if value == 1 {
                started.store(true, Ordering::SeqCst);
            }
            tokio::task::block_in_place(|| {
                while !gate.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(1));
                }
            });
            if value == 1 {
                first_returned.store(true, Ordering::SeqCst);
            }
        }
        value * 10
    });
    lithia::flush().await;
    n.set(1);
    lithia_test::until(|| started.load(Ordering::SeqCst)).await;
    n.set(2); // once delivered, supersedes the run computing 10; its successor waits at the second gate
    let superseded: Option<lithia_test::Captured> =
        lithia_test::find(Level::TRACE, &["run superseded or aborted", &site], Duration::from_secs(10)).await;
    release_first.store(true, Ordering::SeqCst);
    assert!(superseded.is_some(), "the write of 2 supersedes the run computing 10");
    lithia_test::until(|| first_returned.load(Ordering::SeqCst)).await;
    tokio::time::sleep(Duration::from_millis(30)).await; // time for the returned item to be (wrongly) published
    let seen: u32 = *slow.get();
    release_second.store(true, Ordering::SeqCst); // before asserting: a blocked body would hold the runtime open
    assert_eq!(seen, 0, "a superseded run's item is discarded");
    lithia::flush().await;
    assert_eq!(*slow.get(), 20);
}

#[lithia::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_function_a_body_defers_counts_in_the_scope_of_the_write_it_serves() {
    let x: SourceNode<u32> = node!(0);
    let done: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let _deferring: DerivedNode<()> = node!(|x| {
        if *x.get() > 0 {
            let done: Arc<AtomicBool> = done.clone();
            lithia::defer(move || {
                std::thread::sleep(Duration::from_millis(40));
                done.store(true, Ordering::SeqCst);
            });
        }
    });
    lithia::flush().await;
    let seen: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let (writer, flag, out): (SourceNode<u32>, Arc<AtomicBool>, Arc<AtomicBool>) =
        (x.clone(), done.clone(), seen.clone());
    lithia::spawn(async move {
        writer.set(1);
        lithia::flush().await; // waits for what the run it started deferred
        out.store(flag.load(Ordering::SeqCst), Ordering::SeqCst);
    })
    .await
    .unwrap();
    assert!(seen.load(Ordering::SeqCst), "the task's flush returned before the deferred function ran");
}

#[lithia::test(flavor = "multi_thread", worker_threads = 2)]
async fn children_linked_while_a_parent_runs_wait_for_it_wherever_it_sits() {
    let y: SourceNode<u32> = node!(1);
    let a: SourceNode<u32> = node!(0);
    let started: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let slow: DerivedNode<u32> = node!(|a| {
        let value: u32 = *a.get();
        if value > 0 {
            started.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(100));
        }
        value
    });
    let x: SourceNode<u32> = node!(1);
    lithia::flush().await;
    a.set(5);
    lithia_test::until(|| started.load(Ordering::SeqCst)).await;
    // Made while `slow` runs, each child waits for it: its first parent in one, its second in the other.
    let first: DerivedNode<u32> = node!(|slow, x| *slow.get() + *x.get());
    let second: DerivedNode<u32> = node!(|y, slow| *y.get() + *slow.get());
    lithia::flush().await;
    assert_eq!((*first.get(), *second.get()), (6, 6));
}
