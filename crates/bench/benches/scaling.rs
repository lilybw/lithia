// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! How lithia's shared state behaves as threads are added, for the 8–64 cores it targets (spec §15).
//! Run on purpose: `cargo bench -p lithia-bench --bench scaling`; `LITHIA_BENCH_THREADS=1,8,64` picks counts.

#![allow(clippy::type_complexity)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use lithia::{DerivedNode, SourceNode, node};

/// One benchmark across thread counts: a cell per count.
struct Scaling {
    name: &'static str,
    reads: &'static str,
    measure: fn(usize) -> String,
}

const BENCHES: &[Scaling] = &[
    Scaling {
        name: "last-handle",
        reads: "Last-handle drops, one per thread at once (the sweep lock): per drop; flat is no contention",
        measure: last_handle_drops,
    },
    Scaling {
        name: "sweeps",
        reads: "Each thread creates 2 000 effects support holds, then drops its hub at once (the sweep lock): per creation / per sweep",
        measure: sweeps,
    },
    Scaling {
        name: "clock",
        reads: "Writes to a node of each thread's own, all at once (the version clock): per write; flat is no contention",
        measure: clock,
    },
    Scaling {
        name: "readers",
        reads: "A 10-node chain rerun while that many threads read it (the value cells): per write",
        measure: readers,
    },
    Scaling {
        name: "fan-out",
        reads: "One write to 1 000 children on that many workers (runner tasks): per write; falls as workers help",
        measure: fan_out,
    },
    Scaling {
        name: "fan-in",
        reads: "64 writers into one join on that many workers (the scheduling locks): per write",
        measure: fan_in,
    },
    Scaling {
        name: "rebuild",
        reads: "A rerun rebuilding 100 children on that many workers (generations, drops): per rerun",
        measure: rebuild,
    },
];

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

/// `LITHIA_BENCH_THREADS`, or the powers of two up to this machine's parallelism and that count itself.
fn thread_counts() -> Vec<usize> {
    if let Ok(list) = std::env::var("LITHIA_BENCH_THREADS") {
        let counts: Vec<usize> =
            list.split(',').filter_map(|n: &str| n.trim().parse::<usize>().ok()).filter(|n: &usize| *n > 0).collect();
        if !counts.is_empty() {
            return counts;
        }
    }
    let cores: usize = std::thread::available_parallelism().map_or(4, |n: std::num::NonZeroUsize| n.get());
    let mut counts: Vec<usize> =
        std::iter::successors(Some(1usize), |n: &usize| Some(n * 2)).take_while(|n: &usize| *n < cores).collect();
    counts.push(cores);
    counts
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

/// The latency of a last-handle drop on a node support still holds, from `threads` bodies at once.
fn last_handle_drops(threads: usize) -> String {
    const DROPS: u64 = 100_000;
    let rt: tokio::runtime::Runtime = runtime(threads);
    let each: Duration = rt.block_on(lithia_task(async move {
        let total: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
        let mut triggers: Vec<SourceNode<u64>> = Vec::new();
        let mut sources: Vec<SourceNode<u64>> = Vec::new();
        for _ in 0..threads {
            let s: SourceNode<u64> = node!(1);
            let m: DerivedNode<u64> = node!(|s| *s.get());
            let trigger: SourceNode<u64> = node!(0);
            let total: Arc<AtomicU64> = total.clone();
            node!(|trigger| {
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
    }));
    per(each, DROPS)
}

/// Each of `threads` tasks creates effects support holds on a hub of its own, then all drop their hubs.
fn sweeps(threads: usize) -> String {
    const WATCHERS: u64 = 2_000;
    let rt: tokio::runtime::Runtime = runtime(threads);
    let (create, sweep): (Duration, Duration) = rt.block_on(lithia_task(async move {
        let barrier: Arc<tokio::sync::Barrier> = Arc::new(tokio::sync::Barrier::new(threads));
        let times: Arc<Mutex<Vec<(Duration, Duration)>>> = Arc::new(Mutex::new(Vec::new()));
        let tasks: Vec<lithia::JoinHandle<()>> = (0..threads)
            .map(|_| {
                let barrier: Arc<tokio::sync::Barrier> = barrier.clone();
                let times: Arc<Mutex<Vec<(Duration, Duration)>>> = times.clone();
                lithia::spawn(async move {
                    let hub: SourceNode<u64> = node!(0);
                    barrier.wait().await;
                    let start: Instant = Instant::now();
                    for _ in 0..WATCHERS {
                        node!(|hub| {
                            let _ = hub.get();
                        });
                    }
                    let create: Duration = start.elapsed();
                    lithia::flush().await;
                    barrier.wait().await;
                    let start: Instant = Instant::now();
                    drop(hub); // one sweep lets go of every watcher
                    let sweep: Duration = start.elapsed();
                    times.lock().unwrap().push((create, sweep));
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap();
        }
        let (mut create, mut sweep): (Duration, Duration) = (Duration::ZERO, Duration::ZERO);
        for (c, s) in times.lock().unwrap().iter() {
            create += *c;
            sweep += *s;
        }
        (create / threads as u32, sweep / threads as u32)
    }));
    format!("{} / {}", per(create, WATCHERS), per(sweep, 1))
}

/// Each of `threads` writers writes a node of its own, all starting together.
fn clock(threads: usize) -> String {
    const WRITES: u64 = 20_000;
    let rt: tokio::runtime::Runtime = runtime(threads);
    let total: Duration = rt.block_on(async move {
        let barrier: Arc<tokio::sync::Barrier> = Arc::new(tokio::sync::Barrier::new(threads));
        let tasks: Vec<tokio::task::JoinHandle<Duration>> = (0..threads)
            .map(|_| {
                let barrier: Arc<tokio::sync::Barrier> = barrier.clone();
                tokio::spawn(async move {
                    let node: SourceNode<u64> = node!(0);
                    barrier.wait().await;
                    let start: Instant = Instant::now();
                    for round in 1..=WRITES {
                        node.set(round);
                    }
                    start.elapsed()
                })
            })
            .collect();
        let mut total: Duration = Duration::ZERO;
        for task in tasks {
            total += task.await.unwrap();
        }
        total
    });
    per(total / threads as u32, WRITES)
}

/// A chain of derived nodes rerun by writes while `threads` other threads read every node of it.
fn readers(threads: usize) -> String {
    const LENGTH: usize = 10;
    const WRITES: u64 = 2_000;
    let rt: tokio::runtime::Runtime = runtime(2);
    rt.block_on(lithia_task(async move {
        let source: SourceNode<u64> = node!(0);
        let mut chain: Vec<DerivedNode<u64>> = vec![node!(|source| *source.get() + 1)];
        for _ in 1..LENGTH {
            let previous: DerivedNode<u64> = chain.last().unwrap().clone();
            chain.push(node!(|previous| *previous.get() + 1));
        }
        lithia::flush().await;
        let stop: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
        let started: Arc<Barrier> = Arc::new(Barrier::new(threads + 1));
        let reading: Vec<std::thread::JoinHandle<u64>> = (0..threads)
            .map(|_| {
                let (chain, stop, started): (Vec<DerivedNode<u64>>, Arc<AtomicBool>, Arc<Barrier>) =
                    (chain.clone(), stop.clone(), started.clone());
                std::thread::spawn(move || {
                    started.wait();
                    let mut sum: u64 = 0;
                    while !stop.load(Ordering::Relaxed) {
                        for link in &chain {
                            sum = sum.wrapping_add(*link.get());
                        }
                    }
                    sum
                })
            })
            .collect();
        started.wait();
        let start: Instant = Instant::now();
        for round in 1..=WRITES {
            source.set(round);
            lithia::flush().await;
        }
        let elapsed: Duration = start.elapsed();
        stop.store(true, Ordering::Relaxed);
        for reader in reading {
            std::hint::black_box(reader.join().unwrap());
        }
        assert_eq!(*chain.last().unwrap().get(), WRITES + LENGTH as u64);
        per(elapsed, WRITES)
    }))
}

/// A source with 1 000 children, written on a runtime of `threads` workers.
fn fan_out(threads: usize) -> String {
    const WIDTH: usize = 1_000;
    const WRITES: u64 = 100;
    let rt: tokio::runtime::Runtime = runtime(threads);
    let elapsed: Duration = rt.block_on(lithia_task(async {
        let source: SourceNode<u64> = node!(0);
        let children: Vec<DerivedNode<u64>> = (0..WIDTH).map(|_| node!(|source| *source.get())).collect();
        lithia::flush().await;
        let start: Instant = Instant::now();
        for round in 1..=WRITES {
            source.set(round);
            lithia::flush().await;
        }
        let elapsed: Duration = start.elapsed();
        drop(children);
        elapsed
    }));
    per(elapsed, WRITES)
}

/// 64 parents, each written from a task of its own, into a balanced tree of 8-way joins.
fn fan_in(threads: usize) -> String {
    const PARENTS: usize = 64;
    const WRITES: u64 = 200;
    let rt: tokio::runtime::Runtime = runtime(threads);
    let elapsed: Duration = rt.block_on(lithia_task(async {
        let parents: Vec<SourceNode<u64>> = (0..PARENTS).map(|_| node!(0u64)).collect();
        let level: Vec<DerivedNode<u64>> = parents
            .chunks(8)
            .map(|group: &[SourceNode<u64>]| {
                let [a, b, c, d, e, f, g, h]: [SourceNode<u64>; 8] = std::array::from_fn(|i: usize| group[i].clone());
                node!(|a, b, c, d, e, f, g, h| {
                    *a.get() + *b.get() + *c.get() + *d.get() + *e.get() + *f.get() + *g.get() + *h.get()
                })
            })
            .collect();
        let [a, b, c, d, e, f, g, h]: [DerivedNode<u64>; 8] = std::array::from_fn(|i: usize| level[i].clone());
        let join: DerivedNode<u64> = node!(|a, b, c, d, e, f, g, h| *a.get()
            + *b.get()
            + *c.get()
            + *d.get()
            + *e.get()
            + *f.get()
            + *g.get()
            + *h.get());
        lithia::flush().await;
        let start: Instant = Instant::now();
        let tasks: Vec<lithia::JoinHandle<()>> = parents
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
        start.elapsed()
    }));
    per(elapsed, PARENTS as u64 * WRITES)
}

/// A creator rebuilding 100 children on every rerun, on a runtime of `threads` workers.
fn rebuild(threads: usize) -> String {
    const CHILDREN: usize = 100;
    const ROUNDS: u64 = 50;
    let rt: tokio::runtime::Runtime = runtime(threads);
    let elapsed: Duration = rt.block_on(lithia_task(async {
        let tick: SourceNode<u64> = node!(0);
        let k: SourceNode<u64> = node!(0);
        node!(|tick| {
            let _ = tick.get();
            for _ in 0..CHILDREN {
                node!(|k| {
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
    per(elapsed, ROUNDS)
}

fn main() {
    // Cargo passes `--bench`; nothing else is read from the command line.
    let counts: Vec<usize> = thread_counts();
    let only: Option<String> = std::env::var("LITHIA_BENCH").ok();
    let cores: usize = std::thread::available_parallelism().map_or(0, |n: std::num::NonZeroUsize| n.get());
    println!(
        "Host: {} {}, {cores} hardware threads. Threads per cell: {:?}.\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        counts
    );
    let header: Vec<String> = counts.iter().map(|n: &usize| n.to_string()).collect();
    println!("| Benchmark | {} | What a cell is |", header.join(" | "));
    println!("|---|{}---|", "---|".repeat(counts.len()));
    for bench in BENCHES {
        if only.as_deref().is_some_and(|list: &str| !list.split(',').any(|wanted: &str| wanted.trim() == bench.name)) {
            continue;
        }
        let cells: Vec<String> = counts.iter().map(|n: &usize| (bench.measure)(*n)).collect();
        println!("| {} | {} | {} |", bench.name, cells.join(" | "), bench.reads);
    }
}
