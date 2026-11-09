//! Benchmarks of spec §15: each prints what it measures and the decision it informs.
//! Run with `cargo bench -p lithia-bench`; numbers are indicative, not a regression gate.

#![allow(clippy::type_complexity)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lithia::{DerivedNode, SourceNode, effect};

/// One result row.
struct Row {
    name: &'static str,
    result: String,
    informs: &'static str,
}

fn runtime(threads: usize) -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(threads).enable_all().build().unwrap()
}

fn per(duration: Duration, count: u64) -> String {
    let nanos: f64 = duration.as_nanos() as f64 / count.max(1) as f64;
    if nanos >= 1_000_000.0 {
        format!("{:.2} ms", nanos / 1_000_000.0)
    } else if nanos >= 1_000.0 {
        format!("{:.2} µs", nanos / 1_000.0)
    } else {
        format!("{nanos:.0} ns")
    }
}

/// One task per run against running a cheap body inline (informs the §11.4 step 4 fast path).
fn spawn_overhead() -> Row {
    const LENGTH: u64 = 1_000;
    const ROUNDS: u64 = 20;
    let rt: tokio::runtime::Runtime = runtime(2);
    let elapsed: Duration = rt.block_on(lithia_task(async {
        let source: SourceNode<u64> = effect!(0);
        let mut tail: DerivedNode<u64> = effect!(|source| *source.get() + 1);
        let mut chain: Vec<DerivedNode<u64>> = Vec::new();
        for _ in 1..LENGTH {
            let previous: DerivedNode<u64> = tail.clone();
            chain.push(previous.clone());
            tail = effect!(|previous| *previous.get() + 1);
        }
        lithia::flush().await;
        let start: Instant = Instant::now();
        for round in 1..=ROUNDS {
            source.set(round);
            lithia::flush().await;
        }
        assert_eq!(*tail.get(), ROUNDS + LENGTH);
        start.elapsed()
    }));
    let bodies: Vec<fn(u64) -> u64> = (0..LENGTH).map(|_| (|x: u64| x + 1) as fn(u64) -> u64).collect();
    let start: Instant = Instant::now();
    let mut total: u64 = 0;
    for round in 1..=ROUNDS {
        total = total.wrapping_add(bodies.iter().fold(round, |acc: u64, f: &fn(u64) -> u64| f(acc)));
    }
    let inline: Duration = start.elapsed();
    std::hint::black_box(total);
    Row {
        name: "Spawn overhead",
        result: format!(
            "{} per run spawned vs {} per body inline",
            per(elapsed, ROUNDS * LENGTH),
            per(inline, ROUNDS * LENGTH)
        ),
        informs: "Whether the fast path (11.4 step 4) belongs in v0",
    }
}

/// Framing a region, per write in detached tasks and per batch in lithia tasks.
fn framing() -> Row {
    const WIDTH: usize = 1_000;
    const WRITES: u64 = 100;
    let rt: tokio::runtime::Runtime = runtime(2);
    let (fan, batched, detached): (Duration, Duration, Duration) = rt.block_on(lithia_task(async {
        let source: SourceNode<u64> = effect!(0);
        let children: Vec<DerivedNode<u64>> = (0..WIDTH).map(|_| effect!(|source| *source.get())).collect();
        lithia::flush().await;
        let start: Instant = Instant::now();
        for round in 1..=WRITES {
            source.set(round);
            lithia::flush().await;
        }
        let fan: Duration = start.elapsed();
        drop(children);

        let narrow: SourceNode<u64> = effect!(0);
        let _child: DerivedNode<u64> = effect!(|narrow| *narrow.get());
        lithia::flush().await;
        let start: Instant = Instant::now();
        for round in 1..=WRITES {
            narrow.set(round); // one batch: delivered at the flush
        }
        lithia::flush().await;
        let batched: Duration = start.elapsed();
        let detached_source: SourceNode<u64> = narrow.clone();
        let start: Instant = Instant::now();
        tokio::spawn(async move {
            for round in 1..=WRITES {
                detached_source.set(round + 1_000); // one batch per write
            }
            lithia::flush().await;
        })
        .await
        .unwrap();
        (fan, batched, start.elapsed())
    }));
    Row {
        name: "Framing",
        result: format!(
            "fan-out of {WIDTH}: {} per write; {WRITES} writes: {} batched in a lithia task vs {} one batch per write",
            per(fan, WRITES),
            per(batched, 1),
            per(detached, 1)
        ),
        informs: "Whether framing needs caching, and batching advice (6.2)",
    }
}

/// Many parents releasing one join at once.
fn fan_in() -> Row {
    const PARENTS: usize = 64;
    const WRITES: u64 = 200;
    let rt: tokio::runtime::Runtime = runtime(4);
    let (elapsed, runs): (Duration, u64) = rt.block_on(lithia_task(async {
        let parents: Vec<SourceNode<u64>> = (0..PARENTS).map(|_| effect!(0u64)).collect();
        let runs: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
        // A join over 64 parents, built as a balanced tree of 8-way joins.
        let mut level: Vec<DerivedNode<u64>> = Vec::new();
        for group in parents.chunks(8) {
            let (a, b, c, d, e, f, g, h): (
                SourceNode<u64>,
                SourceNode<u64>,
                SourceNode<u64>,
                SourceNode<u64>,
                SourceNode<u64>,
                SourceNode<u64>,
                SourceNode<u64>,
                SourceNode<u64>,
            ) = (
                group[0].clone(),
                group[1].clone(),
                group[2].clone(),
                group[3].clone(),
                group[4].clone(),
                group[5].clone(),
                group[6].clone(),
                group[7].clone(),
            );
            level.push(effect!(|a, b, c, d, e, f, g, h| {
                *a.get() + *b.get() + *c.get() + *d.get() + *e.get() + *f.get() + *g.get() + *h.get()
            }));
        }
        let (a, b, c, d, e, f, g, h): (
            DerivedNode<u64>,
            DerivedNode<u64>,
            DerivedNode<u64>,
            DerivedNode<u64>,
            DerivedNode<u64>,
            DerivedNode<u64>,
            DerivedNode<u64>,
            DerivedNode<u64>,
        ) = (
            level[0].clone(),
            level[1].clone(),
            level[2].clone(),
            level[3].clone(),
            level[4].clone(),
            level[5].clone(),
            level[6].clone(),
            level[7].clone(),
        );
        let counter: Arc<AtomicU64> = runs.clone();
        let join: DerivedNode<u64> = effect!(|a, b, c, d, e, f, g, h| {
            counter.fetch_add(1, Ordering::Relaxed);
            *a.get() + *b.get() + *c.get() + *d.get() + *e.get() + *f.get() + *g.get() + *h.get()
        });
        lithia::flush().await;
        let start: Instant = Instant::now();
        let tasks: Vec<tokio::task::JoinHandle<()>> = parents
            .iter()
            .map(|parent: &SourceNode<u64>| {
                let parent: SourceNode<u64> = parent.clone();
                lithia::spawn(async move {
                    for round in 1..=WRITES {
                        parent.set(round);
                        tokio::task::yield_now().await;
                    }
                    lithia::flush().await;
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap();
        }
        lithia::test::settled(&join).await;
        assert_eq!(*join.get(), PARENTS as u64 * WRITES);
        (start.elapsed(), runs.load(Ordering::Relaxed))
    }));
    Row {
        name: "Fan-in contention",
        result: format!(
            "{} writes into a {PARENTS}-parent join: {} per write, join ran {runs} times",
            PARENTS as u64 * WRITES,
            per(elapsed, PARENTS as u64 * WRITES)
        ),
        informs: "The lock design (11.4 step 1)",
    }
}

/// Parallel writes to independent nodes all take stamps from one clock.
fn clock_contention() -> Row {
    const WRITES: u64 = 20_000;
    let mut results: Vec<String> = Vec::new();
    for threads in [1usize, 2, 4] {
        let rt: tokio::runtime::Runtime = runtime(threads);
        let elapsed: Duration = rt.block_on(async {
            let start: Instant = Instant::now();
            let tasks: Vec<tokio::task::JoinHandle<()>> = (0..threads)
                .map(|_| {
                    tokio::spawn(async move {
                        let node: SourceNode<u64> = effect!(0);
                        for round in 1..=WRITES {
                            node.set(round);
                        }
                    })
                })
                .collect();
            for task in tasks {
                task.await.unwrap();
            }
            start.elapsed()
        });
        results.push(format!("{threads} writer(s): {} per write", per(elapsed, WRITES * threads as u64)));
    }
    Row {
        name: "Clock contention",
        result: results.join("; "),
        informs: "Falling back to one seen version per parent (5, 7.2)",
    }
}

/// Single-threaded propagation against recomputing under one global lock.
fn protocol_overhead() -> Row {
    const LAYERS: usize = 50;
    const ROUNDS: u64 = 200;
    let rt: tokio::runtime::Runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let lithia_time: Duration = rt.block_on(lithia_task(async {
        let source: SourceNode<u64> = effect!(0);
        let mut left: DerivedNode<u64> = effect!(|source| *source.get() + 1);
        let mut right: DerivedNode<u64> = effect!(|source| *source.get() + 2);
        let mut keep: Vec<DerivedNode<u64>> = Vec::new();
        for _ in 0..LAYERS {
            let (l, r): (DerivedNode<u64>, DerivedNode<u64>) = (left.clone(), right.clone());
            keep.push(l.clone());
            keep.push(r.clone());
            left = effect!(|l, r| *l.get() + *r.get());
            right = effect!(|l, r| (*l.get()).max(*r.get()));
        }
        lithia::flush().await;
        let start: Instant = Instant::now();
        for round in 1..=ROUNDS {
            source.set(round);
            lithia::flush().await;
        }
        std::hint::black_box(*left.get());
        start.elapsed()
    }));
    // Baseline: the same diamond chain recomputed in order under one global lock.
    let values: Mutex<Vec<(u64, u64)>> = Mutex::new(vec![(0, 0); LAYERS + 1]);
    let start: Instant = Instant::now();
    for round in 1..=ROUNDS {
        let mut guard: std::sync::MutexGuard<'_, Vec<(u64, u64)>> = values.lock().unwrap();
        guard[0] = (round + 1, round + 2);
        for layer in 1..=LAYERS {
            let (l, r): (u64, u64) = guard[layer - 1];
            guard[layer] = (l + r, l.max(r));
        }
    }
    let baseline: Duration = start.elapsed();
    std::hint::black_box(values.lock().unwrap()[LAYERS]);
    let nodes: u64 = (2 * LAYERS + 2) as u64;
    Row {
        name: "Protocol overhead",
        result: format!(
            "{} per node run vs {} per node under a global lock (MV-RP measured 20–25% for its full machinery)",
            per(lithia_time, ROUNDS * nodes),
            per(baseline, ROUNDS * nodes)
        ),
        informs: "Whether lithia's subset of MV-RP is as cheap as expected (11.4)",
    }
}

/// Runs of an n-way binding mesh after one outside write, against the n² bound.
fn meshes() -> Row {
    let rt: tokio::runtime::Runtime = runtime(2);
    let mut results: Vec<String> = Vec::new();
    for n in [2usize, 3, 4, 6, 8] {
        let runs: u64 = rt.block_on(lithia_task(async move {
            let nodes: Vec<SourceNode<u64>> = (0..n).map(|_| effect!(0u64)).collect();
            let counter: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
            let mut bindings: Vec<DerivedNode<()>> = Vec::new();
            for index in 0..n {
                let me: SourceNode<u64> = nodes[index].clone();
                let others: Vec<SourceNode<u64>> =
                    nodes.iter().enumerate().filter(|(i, _)| *i != index).map(|(_, node)| node.clone()).collect();
                let counter: Arc<AtomicU64> = counter.clone();
                bindings.push(effect!(|me| {
                    counter.fetch_add(1, Ordering::Relaxed);
                    let value: u64 = *me.get();
                    for other in &others {
                        other.set(value);
                    }
                }));
            }
            lithia::flush().await;
            counter.store(0, Ordering::Relaxed);
            nodes[0].set(7);
            lithia::flush().await;
            counter.load(Ordering::Relaxed)
        }));
        results.push(format!("n={n}: {runs} runs (n²={})", n * n));
    }
    Row {
        name: "Writer sets in meshes",
        result: results.join("; "),
        informs: "Whether accumulation needs tuning (7.4)",
    }
}

/// Per-item effects recreated on every run, each replaced value dropped on the blocking pool.
fn drop_churn() -> Row {
    const ROWS: usize = 100;
    const RERUNS: u64 = 100;
    let rt: tokio::runtime::Runtime = runtime(2);
    let elapsed: Duration = rt.block_on(lithia_task(async {
        let version: SourceNode<u64> = effect!(0);
        let rows: Vec<SourceNode<String>> = (0..ROWS).map(|i| effect!(format!("row {i}"))).collect();
        let shared: Arc<Vec<SourceNode<String>>> = Arc::new(rows);
        let _printers: DerivedNode<Vec<DerivedNode<()>>> = effect!(|version| {
            let _ = version.get();
            shared
                .iter()
                .map(|row: &SourceNode<String>| {
                    let row: SourceNode<String> = row.clone();
                    effect!(|row| {
                        std::hint::black_box(row.get().len());
                    })
                })
                .collect()
        });
        lithia::flush().await;
        let start: Instant = Instant::now();
        for round in 1..=RERUNS {
            version.set(round);
            lithia::flush().await;
        }
        start.elapsed()
    }));
    Row {
        name: "Drop churn",
        result: format!("{ROWS} per-item effects recreated: {} per rerun", per(elapsed, RERUNS)),
        informs: "Batching drops (10.2, 10.3)",
    }
}

/// Typical rerun rates of a healthy graph under continuous input.
fn rerun_rates() -> Row {
    let rt: tokio::runtime::Runtime = runtime(2);
    let rate: f64 = rt.block_on(lithia_task(async {
        let input: SourceNode<u64> = effect!(0);
        let runs: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
        let counter: Arc<AtomicU64> = runs.clone();
        let _view: DerivedNode<u64> = effect!(|input| {
            counter.fetch_add(1, Ordering::Relaxed);
            *input.get() * 2
        });
        lithia::flush().await;
        let start: Instant = Instant::now();
        let mut round: u64 = 0;
        while start.elapsed() < Duration::from_millis(500) {
            round += 1;
            input.set(round);
            lithia::flush().await;
        }
        runs.load(Ordering::Relaxed) as f64 / start.elapsed().as_secs_f64()
    }));
    Row {
        name: "Rerun rates",
        result: format!("one node driven as fast as flush allows: {rate:.0} runs/s (warning threshold default 1000/s)"),
        informs: "The default threshold for the rerun-rate warning (12, 17)",
    }
}

/// Clone and drop of an outside handle, which counts anchors beside its `Arc`, against a bare `Arc`.
fn handle_clone() -> Row {
    const COUNT: u64 = 2_000_000;
    let rt: tokio::runtime::Runtime = runtime(1);
    let (handle, bare, read): (Duration, Duration, Duration) = rt.block_on(async {
        let n: SourceNode<u64> = effect!(0);
        let start: Instant = Instant::now();
        for _ in 0..COUNT {
            std::hint::black_box(n.clone());
        }
        let handle: Duration = start.elapsed();
        let arc: Arc<u64> = Arc::new(0);
        let start: Instant = Instant::now();
        for _ in 0..COUNT {
            std::hint::black_box(arc.clone());
        }
        let bare: Duration = start.elapsed();
        let start: Instant = Instant::now();
        for _ in 0..COUNT {
            std::hint::black_box(n.get());
        }
        (handle, bare, start.elapsed())
    });
    Row {
        name: "Handles (v0.3.0)",
        result: format!(
            "clone and drop {} with the anchoring count vs {} for a bare `Arc`; a read {}",
            per(handle, COUNT),
            per(bare, COUNT),
            per(read, COUNT)
        ),
        informs: "Whether the per-node token design is needed (10.1)",
    }
}

/// Creating effects without handles, which support holds, and one sweep letting go of all of them.
fn creation_and_sweep() -> Row {
    const COUNT: u64 = 10_000;
    let rt: tokio::runtime::Runtime = runtime(2);
    let (bound, held, sweep): (Duration, Duration, Duration) = rt.block_on(lithia_task(async {
        let a: SourceNode<u64> = effect!(0);
        let start: Instant = Instant::now();
        let mut kept: Vec<DerivedNode<()>> = Vec::with_capacity(COUNT as usize);
        for _ in 0..COUNT {
            kept.push(effect!(|a| {
                let _ = a.get();
            }));
        }
        let bound: Duration = start.elapsed();
        lithia::flush().await;
        drop(kept);
        let hub: SourceNode<u64> = effect!(0);
        let start: Instant = Instant::now();
        for _ in 0..COUNT {
            effect!(|hub| {
                let _ = hub.get();
            });
        }
        let held: Duration = start.elapsed();
        lithia::flush().await;
        let start: Instant = Instant::now();
        drop(hub); // one sweep lets go of every watcher
        (bound, held, start.elapsed())
    }));
    Row {
        name: "Creation and sweeps (v0.3.0)",
        result: format!(
            "creating a bound effect {}, one held by support {}; dropping the hub's handle lets go of {} watchers in {}",
            per(bound, COUNT),
            per(held, COUNT),
            COUNT,
            per(sweep, 1)
        ),
        informs: "The cost of support at creation and of a sweep over a hub's downstream (10.1)",
    }
}

/// A rerun that rebuilds its children: release of the old generation and its sweep.
fn rebuild_children() -> Row {
    const CHILDREN: usize = 100;
    const ROUNDS: u64 = 50;
    let rt: tokio::runtime::Runtime = runtime(2);
    let elapsed: Duration = rt.block_on(lithia_task(async {
        let tick: SourceNode<u64> = effect!(0);
        let k: SourceNode<u64> = effect!(0);
        effect!(|tick| {
            let _ = tick.get();
            for _ in 0..CHILDREN {
                effect!(|k| {
                    let _ = k.get();
                });
            }
        });
        lithia::flush().await;
        let start: Instant = Instant::now();
        for round in 1..=ROUNDS {
            tick.set(round);
            lithia::flush().await;
        }
        start.elapsed()
    }));
    Row {
        name: "Rebuilding children (v0.3.0)",
        result: format!("{CHILDREN} children rebuilt: {} per rerun", per(elapsed, ROUNDS)),
        informs: "Generations against value-held children (drop churn above; 6.2)",
    }
}

/// The latency of a last-handle drop on a node support still holds, through the helper idiom
/// `helper(&x.accessor())`, on one thread and on two at once (sweep-lock contention).
fn last_handle_drops() -> Row {
    const DROPS: u64 = 100_000;
    let measure = |threads: usize| -> Duration {
        let rt: tokio::runtime::Runtime = runtime(2);
        rt.block_on(lithia_task(async move {
            let total: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
            let mut triggers: Vec<SourceNode<u64>> = Vec::new();
            let mut sources: Vec<SourceNode<u64>> = Vec::new();
            for _ in 0..threads {
                let s: SourceNode<u64> = effect!(1);
                let m: DerivedNode<u64> = effect!(|s| *s.get());
                let trigger: SourceNode<u64> = effect!(0);
                let total: Arc<AtomicU64> = total.clone();
                effect!(|trigger| {
                    if *trigger.get() > 0 {
                        let start: Instant = Instant::now();
                        for _ in 0..DROPS {
                            drop(m.accessor()); // anchors `m` and lets go: a sweep that ends at once
                        }
                        total.fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
                    }
                });
                drop(m);
                sources.push(s);
                triggers.push(trigger);
            }
            lithia::flush().await;
            for trigger in &triggers {
                trigger.set(1);
            }
            lithia::flush().await;
            Duration::from_nanos(total.load(Ordering::Relaxed) / threads as u64)
        }))
    };
    let one: Duration = measure(1);
    let two: Duration = measure(2);
    Row {
        name: "Last-handle drops (v0.3.0)",
        result: format!("{} per drop on one thread, {} on each of two", per(one, DROPS), per(two, DROPS)),
        informs: "The drop-latency budget and the one sweep lock (5.5)",
    }
}

/// Runs a future as a lithia task, so its polls form batches like `#[lithia::main]`.
async fn lithia_task<F>(future: F) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let (tx, rx): (tokio::sync::oneshot::Sender<F::Output>, tokio::sync::oneshot::Receiver<F::Output>) =
        tokio::sync::oneshot::channel();
    lithia::spawn(async move {
        let _ = tx.send(future.await);
    });
    rx.await.expect("the benchmark task panicked")
}

fn main() {
    let rows: Vec<Row> = vec![
        spawn_overhead(),
        framing(),
        fan_in(),
        clock_contention(),
        protocol_overhead(),
        meshes(),
        drop_churn(),
        rerun_rates(),
        handle_clone(),
        creation_and_sweep(),
        rebuild_children(),
        last_handle_drops(),
    ];
    println!("| Benchmark | Result | Informs |");
    println!("|---|---|---|");
    for row in rows {
        println!("| {} | {} | {} |", row.name, row.result, row.informs);
    }
}
