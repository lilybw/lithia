// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! How lithia's shared state behaves as threads are added, for the 8–64 cores it targets (spec §15).
//! Run on purpose: `cargo bench -p lithia-bench --bench scaling`; `LITHIA_BENCH_THREADS=1,8,64` picks counts,
//! `LITHIA_BENCH_PIN=dense|spread` pins worker threads, and `--cfg lithia_lock_stats` adds the sweep lock's figures.

#![allow(clippy::type_complexity)]

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
        reads: "Each thread creates 2 000 effects support holds, then drops its hub at once: per creation / per drop, sweep and freeing, median (fastest–slowest)",
        measure: sweeps,
    },
    Scaling {
        name: "sweep-10",
        reads: "Each thread drops a hub of 10 effects support holds, all at once: per sweep, median (fastest–slowest thread)",
        measure: sweep_10,
    },
    Scaling {
        name: "sweep-100",
        reads: "The same with 100 effects per hub: per sweep, median (fastest–slowest thread)",
        measure: sweep_100,
    },
    Scaling {
        name: "sweep-kept",
        reads: "2 000 effects per hub, each also reading an anchored source: the sweep walks them all and lets go of none",
        measure: sweep_kept,
    },
    Scaling {
        name: "sweep-turns",
        reads: "2 000 effects per hub, the hubs dropped one at a time: one sweep's cost with that many graphs alive",
        measure: sweep_turns,
    },
    Scaling {
        name: "churn",
        reads: "Each thread creates a hub and 10 effects, flushes and drops the hub, 200 times: per cycle, median (fastest–slowest)",
        measure: churn,
    },
    Scaling {
        name: "create-source",
        reads: "Each thread creates 2 000 plain source nodes, then drops them: per creation / per free, median (fastest–slowest)",
        measure: create_source,
    },
    Scaling {
        name: "create-shared",
        reads: "Each thread creates 2 000 effects on one hub all threads share: per creation, median (fastest–slowest)",
        measure: create_shared,
    },
    Scaling {
        name: "alloc",
        reads: "No lithia: each thread allocates 2 000 node-sized boxes, then frees them: per allocation / per free",
        measure: alloc,
    },
    Scaling {
        name: "counter",
        reads: "No lithia: each thread adds to one shared counter, as every creation does to NodeId's: per addition",
        measure: counter,
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
    let mut builder: tokio::runtime::Builder = tokio::runtime::Builder::new_multi_thread();
    builder.worker_threads(threads).enable_all();
    if let Some(order) = pin_order() {
        // The workers start first; threads of the blocking pool, started later, stay unpinned.
        let started: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
        builder.on_thread_start(move || {
            let index: usize = started.fetch_add(1, Ordering::Relaxed);
            if index < threads {
                core_affinity::set_for_current(order[index % order.len()]);
            }
        });
    }
    builder.build().unwrap()
}

/// The logical CPUs to pin workers to, in order: `LITHIA_BENCH_PIN=dense` takes them as numbered,
/// `spread` every other one first, which on most machines is one per physical core first.
fn pin_order() -> Option<Vec<core_affinity::CoreId>> {
    let mode: String = std::env::var("LITHIA_BENCH_PIN").ok()?;
    let cores: Vec<core_affinity::CoreId> = core_affinity::get_core_ids()?;
    match mode.as_str() {
        "dense" => Some(cores),
        "spread" => Some(cores.iter().step_by(2).chain(cores.iter().skip(1).step_by(2)).copied().collect()),
        _ => None,
    }
}

/// The median and the range of per-thread durations, each divided by `count`.
fn spread(mut each: Vec<Duration>, count: u64) -> String {
    each.sort_unstable();
    if each.is_empty() {
        return "-".to_string();
    }
    let median: Duration = each[each.len() / 2];
    if each.len() == 1 {
        return per(median, count);
    }
    format!("{} ({}–{})", per(median, count), per(each[0], count), per(each[each.len() - 1], count))
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

/// How the hubs of [`hubs`] are dropped.
#[derive(Clone, Copy, PartialEq)]
enum Drops {
    /// All at once.
    Together,
    /// One at a time, so no two sweeps overlap.
    Turns,
}

/// Each of `threads` tasks creates `watchers` effects support holds on a hub of its own, then drops
/// the hub; with `kept`, each effect also reads an anchored source of its task, so the sweep lets go of
/// none. Returns each task's time per creation and its drop's time, which includes freeing.
fn hubs(threads: usize, watchers: u64, kept: bool, drops: Drops) -> (Vec<Duration>, Vec<Duration>) {
    let rt: tokio::runtime::Runtime = runtime(threads);
    rt.block_on(lithia_task(async move {
        let barrier: Arc<tokio::sync::Barrier> = Arc::new(tokio::sync::Barrier::new(threads));
        let turns: Arc<tokio::sync::Mutex<()>> = Arc::new(tokio::sync::Mutex::new(()));
        let times: Arc<Mutex<Vec<(Duration, Duration)>>> = Arc::new(Mutex::new(Vec::new()));
        let tasks: Vec<lithia::JoinHandle<()>> = (0..threads)
            .map(|_| {
                let barrier: Arc<tokio::sync::Barrier> = barrier.clone();
                let turns: Arc<tokio::sync::Mutex<()>> = turns.clone();
                let times: Arc<Mutex<Vec<(Duration, Duration)>>> = times.clone();
                lithia::spawn(async move {
                    let hub: SourceNode<u64> = node!(0);
                    let keep: SourceNode<u64> = node!(0);
                    barrier.wait().await;
                    let start: Instant = Instant::now();
                    for _ in 0..watchers {
                        if kept {
                            node!(|hub, keep| {
                                let _ = (hub.get(), keep.get());
                            });
                        } else {
                            node!(|hub| {
                                let _ = hub.get();
                            });
                        }
                    }
                    let create: Duration = start.elapsed();
                    lithia::flush().await;
                    barrier.wait().await;
                    let turn: Option<tokio::sync::MutexGuard<'_, ()>> =
                        if drops == Drops::Turns { Some(turns.lock().await) } else { None };
                    let start: Instant = Instant::now();
                    drop(hub);
                    let sweep: Duration = start.elapsed();
                    drop(turn);
                    times.lock().unwrap().push((create, sweep));
                    barrier.wait().await;
                    drop(keep); // the kept effects go now, unmeasured
                    lithia::flush().await;
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap();
        }
        let times: Vec<(Duration, Duration)> = times.lock().unwrap().clone();
        (
            times.iter().map(|(c, _): &(Duration, Duration)| *c).collect(),
            times.iter().map(|(_, s): &(Duration, Duration)| *s).collect(),
        )
    }))
}

/// Each of `threads` tasks creates effects support holds on a hub of its own, then all drop their hubs.
fn sweeps(threads: usize) -> String {
    const WATCHERS: u64 = 2_000;
    let (create, sweep): (Vec<Duration>, Vec<Duration>) = hubs(threads, WATCHERS, false, Drops::Together);
    let mean: Duration = create.iter().sum::<Duration>() / create.len().max(1) as u32;
    format!("{} / {}", per(mean, WATCHERS), spread(sweep, 1))
}

fn sweep_10(threads: usize) -> String {
    spread(hubs(threads, 10, false, Drops::Together).1, 1)
}

fn sweep_100(threads: usize) -> String {
    spread(hubs(threads, 100, false, Drops::Together).1, 1)
}

fn sweep_kept(threads: usize) -> String {
    spread(hubs(threads, 2_000, true, Drops::Together).1, 1)
}

fn sweep_turns(threads: usize) -> String {
    spread(hubs(threads, 2_000, false, Drops::Turns).1, 1)
}

/// Runs `body` on `threads` lithia tasks started together and returns each task's elapsed time.
fn on_tasks<F, Fut>(threads: usize, body: F) -> Vec<Duration>
where
    F: Fn(usize) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = Duration> + Send + 'static,
{
    let rt: tokio::runtime::Runtime = runtime(threads);
    rt.block_on(lithia_task(async move {
        let times: Arc<Mutex<Vec<Duration>>> = Arc::new(Mutex::new(Vec::new()));
        let tasks: Vec<lithia::JoinHandle<()>> = (0..threads)
            .map(|index: usize| {
                let times: Arc<Mutex<Vec<Duration>>> = times.clone();
                let body: F = body.clone();
                lithia::spawn(async move {
                    let took: Duration = body(index).await;
                    times.lock().unwrap().push(took);
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap();
        }
        let times: Vec<Duration> = times.lock().unwrap().clone();
        times
    }))
}

/// Each task creates a hub and 10 effects on it, flushes and drops the hub, again and again.
fn churn(threads: usize) -> String {
    const CYCLES: u64 = 200;
    let barrier: Arc<tokio::sync::Barrier> = Arc::new(tokio::sync::Barrier::new(threads));
    let each: Vec<Duration> = on_tasks(threads, move |_: usize| {
        let barrier: Arc<tokio::sync::Barrier> = barrier.clone();
        async move {
            barrier.wait().await;
            let start: Instant = Instant::now();
            for _ in 0..CYCLES {
                let hub: SourceNode<u64> = node!(0);
                for _ in 0..10 {
                    node!(|hub| {
                        let _ = hub.get();
                    });
                }
                lithia::flush().await;
                drop(hub);
            }
            start.elapsed()
        }
    });
    spread(each, CYCLES)
}

/// Each task creates plain source nodes, which link to nothing, then drops them, which frees them.
fn create_source(threads: usize) -> String {
    const NODES: u64 = 2_000;
    let barrier: Arc<tokio::sync::Barrier> = Arc::new(tokio::sync::Barrier::new(threads));
    let frees: Arc<Mutex<Vec<Duration>>> = Arc::new(Mutex::new(Vec::new()));
    let freed: Arc<Mutex<Vec<Duration>>> = frees.clone();
    let each: Vec<Duration> = on_tasks(threads, move |_: usize| {
        let barrier: Arc<tokio::sync::Barrier> = barrier.clone();
        let frees: Arc<Mutex<Vec<Duration>>> = frees.clone();
        async move {
            let mut kept: Vec<SourceNode<u64>> = Vec::with_capacity(NODES as usize);
            barrier.wait().await;
            let start: Instant = Instant::now();
            for value in 0..NODES {
                kept.push(node!(value));
            }
            let took: Duration = start.elapsed();
            barrier.wait().await;
            let start: Instant = Instant::now();
            drop(kept);
            frees.lock().unwrap().push(start.elapsed());
            took
        }
    });
    let frees: Vec<Duration> = freed.lock().unwrap().clone();
    format!("{} / {}", spread(each, NODES), spread(frees, NODES))
}

/// Every task creates effects on one hub they all share.
fn create_shared(threads: usize) -> String {
    const WATCHERS: u64 = 2_000;
    let barrier: Arc<tokio::sync::Barrier> = Arc::new(tokio::sync::Barrier::new(threads));
    let hub: SourceNode<u64> = node!(0);
    let each: Vec<Duration> = on_tasks(threads, move |_: usize| {
        let barrier: Arc<tokio::sync::Barrier> = barrier.clone();
        let hub: SourceNode<u64> = hub.clone();
        async move {
            barrier.wait().await;
            let start: Instant = Instant::now();
            for _ in 0..WATCHERS {
                node!(|hub| {
                    let _ = hub.get();
                });
            }
            start.elapsed()
        }
    });
    spread(each, WATCHERS)
}

/// About the size of a node record, so the allocator sees what creations ask of it.
type NodeSized = [u64; 112];

/// No lithia: each task allocates boxes the size of a node, then frees them.
fn alloc(threads: usize) -> String {
    const BOXES: u64 = 2_000;
    let barrier: Arc<tokio::sync::Barrier> = Arc::new(tokio::sync::Barrier::new(threads));
    let frees: Arc<Mutex<Vec<Duration>>> = Arc::new(Mutex::new(Vec::new()));
    let freed: Arc<Mutex<Vec<Duration>>> = frees.clone();
    let each: Vec<Duration> = on_tasks(threads, move |_: usize| {
        let barrier: Arc<tokio::sync::Barrier> = barrier.clone();
        let frees: Arc<Mutex<Vec<Duration>>> = frees.clone();
        async move {
            let mut boxes: Vec<Box<NodeSized>> = Vec::with_capacity(BOXES as usize);
            barrier.wait().await;
            let start: Instant = Instant::now();
            for value in 0..BOXES {
                boxes.push(std::hint::black_box(Box::new([value; 112])));
            }
            let took: Duration = start.elapsed();
            let start: Instant = Instant::now();
            drop(std::hint::black_box(boxes));
            frees.lock().unwrap().push(start.elapsed());
            took
        }
    });
    let frees: Vec<Duration> = freed.lock().unwrap().clone();
    format!("{} / {}", spread(each, BOXES), spread(frees, BOXES))
}

/// One counter every task adds to, as every creation adds to the `NodeId` counter.
static SHARED: AtomicU64 = AtomicU64::new(0);

/// No lithia: each task adds to [`SHARED`] in a loop.
fn counter(threads: usize) -> String {
    const ADDS: u64 = 20_000;
    let barrier: Arc<tokio::sync::Barrier> = Arc::new(tokio::sync::Barrier::new(threads));
    let each: Vec<Duration> = on_tasks(threads, move |_: usize| {
        let barrier: Arc<tokio::sync::Barrier> = barrier.clone();
        async move {
            barrier.wait().await;
            let start: Instant = Instant::now();
            for _ in 0..ADDS {
                std::hint::black_box(SHARED.fetch_add(1, Ordering::SeqCst));
            }
            start.elapsed()
        }
    });
    spread(each, ADDS)
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

/// A cell, followed in a build with `--cfg lithia_lock_stats` by what the sweep lock went through
/// while it was measured: acquisitions, mean and longest hold, mean wait.
fn with_lock_stats(measure: impl FnOnce() -> String) -> String {
    #[cfg(lithia_lock_stats)]
    {
        let _ = lithia::__private::sweep_lock_stats::take();
        let cell: String = measure();
        let stats: lithia::__private::sweep_lock_stats::LockStats = lithia::__private::sweep_lock_stats::take();
        format!(
            "{cell} [{} takes, held {} / {} max, waited {}]",
            stats.acquired,
            per(stats.held, stats.acquired),
            per(stats.longest, 1),
            per(stats.waited, stats.acquired)
        )
    }
    #[cfg(not(lithia_lock_stats))]
    measure()
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
        let cells: Vec<String> = counts.iter().map(|n: &usize| with_lock_stats(|| (bench.measure)(*n))).collect();
        println!("| {} | {} | {} |", bench.name, cells.join(" | "), bench.reads);
    }
}
