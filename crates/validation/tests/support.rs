// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Validation cases AI–BC: how long nodes live without handles (spec §10, §13).
//! Every case runs under the workspace's test timeout (`.cargo/config.toml`).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lithia::{Accessor, DerivedNode, SourceNode, Status, node};
use lithia_test::until;
use tokio::sync::mpsc;

/// Whether a warning containing `text` and naming `line` of this file arrives within `limit`.
async fn warned(text: &str, line: u32, limit: Duration) -> bool {
    lithia_test::warned("support.rs", text, line, limit).await
}

const FIRST_RUN: &str = "freed before its first run";
const UNREAD: &str = "nothing reads it";

fn bump(n: &u32) -> u32 {
    n + 1
}

// Case AI: the motivating example. An effect needs no handle; it lives while its parent can change.
#[lithia::test]
async fn case_ai_an_effect_needs_no_handle() {
    let printed: SourceNode<u32> = node!(0);
    let parent: SourceNode<i32> = node!(5);
    node!(|parent| {
        println!("Value is: {}", parent.get());
        printed.update(bump);
    });
    lithia::flush().await;
    parent.set(6);
    lithia::flush().await;
    assert_eq!(*printed.get(), 2);
}

// Case AI, under a runtime lithia did not start.
#[tokio::test]
async fn case_ai_under_a_plain_tokio_runtime() {
    let printed: SourceNode<u32> = node!(0);
    let parent: SourceNode<i32> = node!(5);
    node!(|parent| {
        println!("Value is: {}", parent.get());
        printed.update(bump);
    });
    lithia::test::settled(&parent).await;
    parent.set(6);
    lithia::test::settled(&parent).await;
    assert_eq!(*printed.get(), 2);
}

// Case AJ: a writer keeps its target's watchers alive.
#[lithia::test]
async fn case_aj_a_writer_keeps_its_targets_watcher_alive() {
    let seen: SourceNode<i32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    let a: SourceNode<i32> = node!(1);
    let x: SourceNode<i32> = node!(0);
    node!(|a| x.set(*a.get() * 10)); // writes `x` while `a` is held
    node!(|x| seen.set(*x.get())); // `x`'s watcher, without a handle
    lithia::flush().await;
    assert_eq!(*seen.get(), 10);
    drop(x);
    assert_eq!(lithia::test::live_nodes().await, base + 4); // nothing holds `x`, but its writer lives
    a.set(2);
    lithia::flush().await;
    assert_eq!(*seen.get(), 20);
    drop(a);
    assert_eq!(lithia::test::live_nodes().await, base); // the writer goes, and the watcher with it
}

// Case AK: a reader keeps nothing alive.
#[lithia::test]
async fn case_ak_a_reader_keeps_nothing_alive() {
    let clock: SourceNode<u64> = node!(0);
    let seen: SourceNode<i32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    {
        let temp: SourceNode<i32> = node!(20);
        node!(|clock| println!("{} at {}", temp.get(), clock.get())); // only reads `temp`
        node!(|temp| seen.set(*temp.get())); // `temp`'s other watcher
        lithia::flush().await;
        assert_eq!(lithia::test::live_nodes().await, base + 3);
    }
    // The reader still holds `temp`, but gives its watcher nothing.
    assert_eq!(lithia::test::live_nodes().await, base + 2);
    clock.set(1);
    lithia::flush().await;
    assert_eq!(lithia::test::live_nodes().await, base + 2);
}

// Case AL: a body rebuilds its children on every run.
#[lithia::test]
async fn case_al_a_body_rebuilds_its_children() {
    let outer: SourceNode<u32> = node!(0);
    let other: SourceNode<u32> = node!(0);
    let inner_runs: SourceNode<u32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    node!(|outer| {
        let _ = outer.get();
        node!(|other| {
            let _ = other.get();
            inner_runs.update(bump);
        });
    });
    lithia::flush().await;
    assert_eq!((*inner_runs.get(), lithia::test::live_nodes().await), (1, base + 2));
    for value in 1..=3u32 {
        outer.set(value);
        lithia::flush().await;
    }
    // Each run created a new child, which ran once; the one before it was freed.
    assert_eq!((*inner_runs.get(), lithia::test::live_nodes().await), (4, base + 2));
    other.set(1);
    lithia::flush().await;
    assert_eq!(*inner_runs.get(), 5); // only the current child reacts
}

// Case AM: a child stored in another node's value outlives its creator's rerun.
#[lithia::test]
async fn case_am_a_stored_child_outlives_the_rerun() {
    let other: SourceNode<u32> = node!(0);
    let runs: SourceNode<u32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    let tick: SourceNode<u32> = node!(0);
    let slot: SourceNode<Option<DerivedNode<()>>> = node!(None);
    let maker: DerivedNode<()> = node!(|tick| {
        if *tick.get() == 0 {
            slot.set(Some(node!(|other| {
                let _ = other.get();
                runs.update(bump);
            })));
        }
    });
    lithia::flush().await;
    assert_eq!((*runs.get(), lithia::test::live_nodes().await), (1, base + 4));
    tick.set(1); // the rerun releases the child; the slot still holds it
    lithia::flush().await;
    other.set(1);
    lithia::flush().await;
    assert_eq!((*runs.get(), lithia::test::live_nodes().await), (2, base + 4));
    slot.set(None);
    lithia::flush().await;
    assert_eq!(lithia::test::live_nodes().await, base + 3);
    drop((tick, slot, maker));
    assert_eq!(lithia::test::live_nodes().await, base);
}

// Case AN: an async source block lives by its handles. A discarded one is freed before it runs, with the warning.
#[lithia::test]
async fn case_an_a_discarded_async_block_never_runs() {
    let _ = lithia_test::events(); // collect from the start
    let ran: SourceNode<bool> = node!(false);
    let base: usize = lithia::test::live_nodes().await;
    let line: u32 = line!() + 1;
    node!(async { ran.set(true) });
    lithia::flush().await;
    assert!(!*ran.get());
    assert_eq!(lithia::test::live_nodes().await, base);
    assert!(warned(FIRST_RUN, line, Duration::from_secs(2)).await);
}

// Case AN, under a runtime lithia did not start: one aborted after its first poll warns too.
#[tokio::test]
async fn case_an_an_aborted_async_block_warns() {
    let _ = lithia_test::events(); // collect from the start
    let started: SourceNode<bool> = node!(false);
    let line: u32 = line!() + 1;
    let block: SourceNode<u32> = node!(async {
        started.set(true);
        std::future::pending::<u32>().await
    });
    until(|| *started.get()).await;
    drop(block);
    assert!(warned(FIRST_RUN, line, Duration::from_secs(2)).await);
}

// Case AN: a bound async block feeding `t` keeps `t`'s watcher alive until the block's handle drops.
#[lithia::test]
async fn case_an_a_bound_feeder_keeps_the_watcher_alive() {
    let seen: SourceNode<u32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    let t: SourceNode<u32> = node!(0);
    node!(|t| seen.set(*t.get()));
    let (tx, mut rx): (mpsc::UnboundedSender<u32>, mpsc::UnboundedReceiver<u32>) = mpsc::unbounded_channel();
    let feeder: SourceNode<()> = node!(async move {
        while let Some(value) = rx.recv().await {
            t.set(value);
        }
    }); // takes the only handle to `t`
    tx.send(5).unwrap();
    until(|| *seen.get() == 5).await;
    assert_eq!(lithia::test::live_nodes().await, base + 3);
    tx.send(6).unwrap();
    until(|| *seen.get() == 6).await;
    drop(feeder);
    assert_eq!(lithia::test::live_nodes().await, base);
}

// Case AN: `node!(async |cfg| loop { … })` runs while `cfg` is held and restarts when it changes.
#[lithia::test]
async fn case_an_a_loop_takes_the_state_it_serves_as_a_parent() {
    let _ = lithia_test::events(); // collect from the start
    let ticks: SourceNode<u32> = node!(0);
    let starts: SourceNode<u32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    let cfg: SourceNode<u64> = node!(1);
    let line: u32 = line!() + 1;
    node!(async |cfg| {
        starts.update(bump);
        loop {
            tokio::time::sleep(Duration::from_millis(*cfg.get())).await;
            ticks.update(bump);
        }
    });
    until(|| *ticks.get() >= 2).await; // polled a second time
    cfg.set(2);
    until(|| *starts.get() == 2 && *ticks.get() >= 4).await; // restarted on the change
    assert_eq!(lithia::test::live_nodes().await, base + 2);
    drop(cfg);
    assert_eq!(lithia::test::live_nodes().await, base);
    let stopped: u32 = *ticks.get();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(*ticks.get(), stopped);
    assert!(!warned(FIRST_RUN, line, Duration::from_millis(50)).await);
}

// Case AO: an idle child on its creator's parent runs exactly once per change.
#[lithia::test]
async fn case_ao_an_idle_old_child_never_runs_again() {
    let a: SourceNode<u32> = node!(0);
    let inner_runs: SourceNode<u32> = node!(0);
    node!(|a| {
        let _ = a.get();
        node!(|a| {
            let _ = a.get();
            inner_runs.update(bump);
        });
    });
    lithia::flush().await;
    assert_eq!(*inner_runs.get(), 1);
    for value in 1..=5u32 {
        a.set(value);
        lithia::flush().await;
        assert_eq!(*inner_runs.get(), value + 1, "the old child was freed before its task existed");
    }
}

// Case AP: captures through a `Vec`, `self` and a generic helper are internal handles.
#[derive(Clone)]
struct Panel {
    count: SourceNode<u32>,
    total: SourceNode<u32>,
}

impl Panel {
    fn watch(&self) {
        let count: SourceNode<u32> = self.count.clone();
        node!(|count| self.total.set(*count.get()));
    }
}

fn watch_generic<R: lithia::Readable<Value = u32> + Clone + Send + Sync + 'static>(n: &R, seen: &SourceNode<u32>) {
    let n: R = n.clone();
    let seen: SourceNode<u32> = seen.clone();
    node!(|n| seen.set(*n.get()));
}

#[lithia::test]
async fn case_ap_captures_are_internal_handles() {
    let seen: SourceNode<u32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    {
        let items: Vec<SourceNode<u32>> = vec![node!(1), node!(2)];
        let a: SourceNode<u32> = node!(0);
        node!(|a| seen.set(*a.get() + items.iter().map(|item: &SourceNode<u32>| *item.get()).sum::<u32>()));
        lithia::flush().await;
        assert_eq!((*seen.get(), lithia::test::live_nodes().await), (3, base + 4));
    }
    assert_eq!(lithia::test::live_nodes().await, base);
    {
        let panel: Panel = Panel { count: node!(7), total: node!(0) };
        panel.watch();
        lithia::flush().await;
        assert_eq!((*panel.total.get(), lithia::test::live_nodes().await), (7, base + 3));
    }
    assert_eq!(lithia::test::live_nodes().await, base);
    {
        let n: SourceNode<u32> = node!(9);
        watch_generic(&n, &seen);
        watch_generic(&n.accessor(), &seen);
        lithia::flush().await;
        assert_eq!((*seen.get(), lithia::test::live_nodes().await), (9, base + 3));
    }
    assert_eq!(lithia::test::live_nodes().await, base);
}

// Case AQ: a write that supersedes an async block releases what the block created.
#[lithia::test]
async fn case_aq_a_superseding_write_releases_the_blocks_creations() {
    let started: SourceNode<bool> = node!(false);
    let base: usize = lithia::test::live_nodes().await;
    let block: SourceNode<u32> = node!(async {
        node!(1u32); // held by the block's run
        started.set(true);
        std::future::pending::<u32>().await
    });
    until(|| *started.get()).await;
    assert_eq!(lithia::test::live_nodes().await, base + 2);
    block.set(5); // stops the block
    lithia::flush().await;
    assert_eq!((*block.get(), lithia::test::live_nodes().await), (5, base + 1));
    drop(block);
    assert_eq!(lithia::test::live_nodes().await, base);
}

// Case AR: a node disposed while its body runs.
#[lithia::test(flavor = "multi_thread", worker_threads = 2)]
async fn case_ar_creations_under_a_disposed_body() {
    let _ = lithia_test::events(); // collect from the start
    let (go_tx, go_rx): (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>) = std::sync::mpsc::channel();
    let (in_tx, in_rx): (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>) = std::sync::mpsc::channel();
    let go_rx: Arc<Mutex<std::sync::mpsc::Receiver<()>>> = Arc::new(Mutex::new(go_rx));
    let free_runs: SourceNode<u32> = node!(0);
    let held_runs: SourceNode<u32> = node!(0);
    let slot: SourceNode<Option<DerivedNode<()>>> = node!(None);
    let k: SourceNode<u32> = node!(1);
    let line: u32 = line!() + 5;
    let body: DerivedNode<()> = node!(|k| {
        let _ = k.get();
        let _ = in_tx.send(());
        let _ = go_rx.lock().unwrap().recv(); // blocks until the test has disposed the node
        node!(|k| {
            let _ = k.get();
            free_runs.update(bump);
        });
        slot.set(Some(node!(|k| {
            let _ = k.get();
            held_runs.update(bump);
        })));
    });
    tokio::task::spawn_blocking(move || in_rx.recv()).await.unwrap().unwrap();
    lithia::dispose(&body);
    go_tx.send(()).unwrap();
    until(|| slot.get().is_some()).await; // the torn-down run's write still applies
    lithia::flush().await; // returns: the held creation was seeded
    until(|| *held_runs.get() == 1).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!((*free_runs.get(), *held_runs.get()), (0, 1));
    assert!(!warned(FIRST_RUN, line, Duration::from_millis(50)).await, "its creator was disposed");
}

// Case AS: a two-way binding of `move` bodies lives while an anchor reaches it.
#[lithia::test]
async fn case_as_move_bodies_take_internal_handles() {
    let seen: SourceNode<i32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    let a: SourceNode<i32> = node!(0, equals = lithia::eq);
    let b: SourceNode<i32> = node!(0, equals = lithia::eq);
    let (from_a, to_b): (SourceNode<i32>, SourceNode<i32>) = (a.clone(), b.clone());
    node!(move |from_a| to_b.set(*from_a.get()));
    let (from_b, to_a): (SourceNode<i32>, SourceNode<i32>) = (b.clone(), a.clone());
    node!(move |from_b| to_a.set(*from_b.get()));
    lithia::flush().await;
    a.set(3);
    lithia::flush().await;
    assert_eq!(*b.get(), 3);
    b.set(4);
    lithia::flush().await;
    assert_eq!(*a.get(), 4);
    drop((a, b));
    assert_eq!(lithia::test::live_nodes().await, base); // the cycle supports nothing

    // A `move` body that takes the only handle to `b` keeps `b`'s watcher alive.
    let a: SourceNode<i32> = node!(1);
    let b: SourceNode<i32> = node!(0);
    node!(|b| seen.set(*b.get()));
    let from_a: SourceNode<i32> = a.clone();
    node!(move |from_a| b.set(*from_a.get() * 2)); // takes the only handle to `b`
    lithia::flush().await;
    a.set(3);
    lithia::flush().await;
    assert_eq!((*seen.get(), lithia::test::live_nodes().await), (6, base + 4));
    drop(a);
    assert_eq!(lithia::test::live_nodes().await, base);
}

// Case AT: per-request memos.
/// A per-request memo that drops its handle; returns the line it is created on.
fn per_request(rates: &SourceNode<f64>, hits: &Arc<AtomicU32>) -> u32 {
    let rates: SourceNode<f64> = rates.clone();
    let hits: Arc<AtomicU32> = hits.clone();
    let line: u32 = line!() + 1;
    let price: DerivedNode<f64> = node!(|rates| {
        hits.fetch_add(1, Ordering::SeqCst);
        *rates.get() * 2.0
    });
    let _ = price.get();
    line
}

async fn handler(rates: &SourceNode<f64>, metrics: &SourceNode<u32>, hang: bool) -> f64 {
    let (rates, metrics): (SourceNode<f64>, SourceNode<u32>) = (rates.clone(), metrics.clone());
    let input: SourceNode<f64> = node!(3.0);
    let price: DerivedNode<f64> = node!(|input, rates| *input.get() * *rates.get()).scoped();
    node!(|price, rates| {
        let _ = (*price.get(), *rates.get());
        metrics.update(bump);
    }); // hangs off `price`, with a second, long-lived parent
    lithia::flush().await;
    if hang {
        std::future::pending::<()>().await;
    }
    *price.get()
}

#[lithia::test]
async fn case_at_per_request_memos() {
    let _ = lithia_test::events(); // collect from the start
    let metrics: SourceNode<u32> = node!(0);
    let hits: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let base: usize = lithia::test::live_nodes().await;
    let rates: SourceNode<f64> = node!(1.5);
    let line: u32 = per_request(&rates, &hits);
    lithia::flush().await;
    assert!(warned(UNREAD, line, Duration::from_secs(2)).await);
    rates.set(2.0);
    lithia::flush().await;
    assert_eq!((hits.load(Ordering::SeqCst), lithia::test::live_nodes().await), (2, base + 2)); // still running
    lithia::dispose(&rates);
    assert_eq!(lithia::test::live_nodes().await, base); // freed with the state it ran on

    // A memo that also writes, in Case T's shape, does not warn.
    let x: SourceNode<i32> = node!(0);
    let doubled: DerivedNode<i32> = node!(|x| *x.get() * 2);
    let line: u32 = line!() + 1;
    node!(|doubled| {
        let over: bool = *doubled.get() > 20;
        if over {
            x.set(10);
        }
        over
    });
    x.set(15);
    lithia::flush().await;
    assert_eq!(*x.get(), 10);
    assert!(!warned(UNREAD, line, Duration::from_millis(50)).await);
    drop((x, doubled));

    // Scoped, the memo and what hangs off it go with the handler.
    let rates: SourceNode<f64> = node!(2.0);
    let base: usize = lithia::test::live_nodes().await;
    assert_eq!(handler(&rates, &metrics, false).await, 6.0);
    assert_eq!((*metrics.get(), lithia::test::live_nodes().await), (1, base));
    rates.set(3.0);
    lithia::flush().await;
    assert_eq!(*metrics.get(), 1);
    let (r, m): (SourceNode<f64>, SourceNode<u32>) = (rates.clone(), metrics.clone());
    let task: tokio::task::JoinHandle<f64> = tokio::spawn(async move { handler(&r, &m, true).await });
    until(|| *metrics.get() >= 2).await; // outside a lithia task, the child may run twice
    task.abort();
    assert!(task.await.is_err());
    let before: u32 = *metrics.get();
    rates.set(4.0);
    lithia::flush().await;
    assert_eq!(*metrics.get(), before); // also freed when the future is cancelled
}

// Case AU: an effect whose parents nobody holds is freed before its first run, with the warning.
fn apply_once(cfg: u32, applied: &SourceNode<u32>) -> u32 {
    let applied: SourceNode<u32> = applied.clone();
    let c: SourceNode<u32> = node!(cfg);
    let line: u32 = line!() + 1;
    node!(|c| applied.set(*c.get()));
    line
}

#[lithia::test]
async fn case_au_an_effect_on_nothing_held_never_runs() {
    let _ = lithia_test::events(); // collect from the start
    let applied: SourceNode<u32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    let line: u32 = apply_once(7, &applied);
    lithia::flush().await;
    assert_eq!((*applied.get(), lithia::test::live_nodes().await), (0, base));
    assert!(warned(FIRST_RUN, line, Duration::from_secs(2)).await);
}

// Case AV: support returns through a handle a body sends out, and passes through a node let go of.
/// What Case AV's body sends out.
type Sent = (Accessor<u32>, Accessor<u32>);

#[lithia::test]
async fn case_av_support_returns_through_a_sent_handle() {
    let trigger: SourceNode<bool> = node!(false);
    let (w1_seen, w2_seen, below_seen): (SourceNode<u32>, SourceNode<u32>, SourceNode<u32>) =
        (node!(0), node!(0), node!(0));
    let (feed, mut fed): (mpsc::UnboundedSender<u32>, mpsc::UnboundedReceiver<u32>) = mpsc::unbounded_channel();
    let (out, mut sent): (mpsc::UnboundedSender<Sent>, mpsc::UnboundedReceiver<Sent>) = mpsc::unbounded_channel();
    let base: usize = lithia::test::live_nodes().await;
    let x: SourceNode<u32> = node!(async move {
        while let Some(value) = fed.recv().await {
            yield value;
        }
    });
    node!(|x| w1_seen.set(*x.get()));
    let m: DerivedNode<u32> = node!(|x| *x.get() * 10);
    node!(|trigger| {
        if *trigger.get() {
            let _ = out.send((x.accessor(), m.accessor())); // reads `x` and `m` only
        }
    }); // its captures keep `x` and `m` alive
    drop(m);
    feed.send(1).unwrap();
    until(|| *w1_seen.get() == 1).await;
    drop(x);
    assert_eq!(lithia::test::live_nodes().await, base + 3); // `x`'s watcher is freed; `m` is let go of
    trigger.set(true);
    let (xa, ma): (Accessor<u32>, Accessor<u32>) = sent.recv().await.unwrap();
    node!(|xa| w2_seen.set(*xa.get()));
    let s: SourceNode<u32> = node!(0);
    node!(|ma, s| below_seen.set(*ma.get() + *s.get()));
    drop(ma);
    lithia::flush().await;
    drop(s); // a sweep from `s`: support still reaches the node below `m` through `m`, which support no longer holds
    lithia::flush().await;
    assert_eq!(lithia::test::live_nodes().await, base + 6);
    feed.send(2).unwrap();
    until(|| *w2_seen.get() == 2 && *below_seen.get() == 20).await;
    assert_eq!(*w1_seen.get(), 1);
    drop(xa);
    assert_eq!(lithia::test::live_nodes().await, base + 3);
}

// Case AW: self-support. A node whose value or captured `Arc` holds its parent's handle stays alive.
struct Conn {
    input: SourceNode<u32>,
}

#[lithia::test]
async fn case_aw_self_support() {
    let runs: SourceNode<u32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    {
        let a: SourceNode<i32> = node!(1);
        let b: SourceNode<i32> = node!(2);
        let _pick: DerivedNode<Option<SourceNode<i32>>> =
            node!(|a, b| Some(if *a.get() > 0 { a.clone() } else { b.clone() }));
        lithia::flush().await;
    }
    assert_eq!(lithia::test::live_nodes().await, base + 3, "the selector holds itself through `a`");
    let base: usize = lithia::test::live_nodes().await;
    {
        let a: SourceNode<i32> = node!(1);
        let b: SourceNode<i32> = node!(2);
        let pick: DerivedNode<Option<SourceNode<i32>>> =
            node!(|a, b| Some(if *a.get() > 0 { a.clone() } else { b.clone() }));
        lithia::flush().await;
        lithia::dispose(&pick);
    }
    assert_eq!(lithia::test::live_nodes().await, base, "dispose frees it");
    {
        let a: SourceNode<i32> = node!(1);
        let b: SourceNode<i32> = node!(2);
        let _pick: DerivedNode<Option<SourceNode<i32>>> =
            node!(|a, b| Some(if *a.get() > 0 { a.clone() } else { b.clone() })).scoped();
        lithia::flush().await;
    }
    assert_eq!(lithia::test::live_nodes().await, base, "scoped, it goes with its handles");
    {
        let conn: Arc<Conn> = Arc::new(Conn { input: node!(0) });
        let input: SourceNode<u32> = conn.input.clone();
        node!(|input| {
            let _ = conn.input.get();
            runs.update(bump);
        });
        lithia::flush().await;
    }
    assert_eq!(lithia::test::live_nodes().await, base + 2, "the effect holds its parent through the `Arc`");
    let base: usize = lithia::test::live_nodes().await;
    {
        let conn: Arc<Conn> = Arc::new(Conn { input: node!(0) });
        let input: SourceNode<u32> = conn.input.clone();
        node!(|input| {
            let _ = conn.input.get();
            runs.update(bump);
        });
        lithia::flush().await;
        lithia::dispose(&input);
    }
    assert_eq!(lithia::test::live_nodes().await, base, "disposing the parent frees it");
}

// Case AX: a creator and a child that may write the creator's parent are freed together.
fn reset(n: &SourceNode<u32>) {
    n.set(0);
}

#[lithia::test]
async fn case_ax_a_child_writing_its_creators_parent() {
    let base: usize = lithia::test::live_nodes().await;
    {
        let p: SourceNode<u32> = node!(1);
        node!(|p| {
            let _ = p.get();
            node!(|p| {
                if *p.get() > 100 {
                    reset(p); // passed to a function: a possible write
                }
            });
        });
        lithia::flush().await;
        p.set(200);
        lithia::flush().await;
        assert_eq!((*p.get(), lithia::test::live_nodes().await), (0, base + 3));
    }
    assert_eq!(lithia::test::live_nodes().await, base);
}

// Case AY: field paths classify the handles of a value captured whole.
#[derive(Clone)]
struct Themed {
    total: SourceNode<u32>,
    theme: SourceNode<String>,
    tx: mpsc::UnboundedSender<u32>,
}

impl Themed {
    fn notify(&self, value: u32) {
        let _ = self.tx.send(value);
    }

    fn precise(&self, count: &SourceNode<u32>) {
        let count: SourceNode<u32> = count.clone();
        node!(|count| {
            self.total.set(*count.get());
            println!("theme {}", self.theme.get());
        });
    }

    fn whole(&self, count: &SourceNode<u32>) {
        let count: SourceNode<u32> = count.clone();
        node!(|count| {
            self.total.set(*count.get());
            println!("theme {}", self.theme.get());
            self.notify(*count.get());
        });
    }

    fn bound(&self, count: &SourceNode<u32>) {
        let count: SourceNode<u32> = count.clone();
        let (total, theme, tx): (&SourceNode<u32>, &SourceNode<String>, &mpsc::UnboundedSender<u32>) =
            (&self.total, &self.theme, &self.tx);
        node!(|count| {
            total.set(*count.get());
            println!("theme {}", theme.get());
            notify(tx, *count.get()); // the body sees its captures by reference
        });
    }
}

fn notify(tx: &mpsc::UnboundedSender<u32>, value: u32) {
    let _ = tx.send(value);
}

/// Starts an effect on `count` from a panel, drops the panel, and counts what is left.
async fn panel_case(start: fn(&Themed, &SourceNode<u32>), theme_kept: bool) {
    let (shown, styled): (SourceNode<u32>, SourceNode<u32>) = (node!(0), node!(0));
    let (tx, _rx): (mpsc::UnboundedSender<u32>, mpsc::UnboundedReceiver<u32>) = mpsc::unbounded_channel();
    let count: SourceNode<u32> = node!(1); // global state
    let base: usize = lithia::test::live_nodes().await;
    {
        let panel: Themed = Themed { total: node!(0), theme: node!("dark".to_string()), tx };
        start(&panel, &count);
        let (total, theme): (SourceNode<u32>, SourceNode<String>) = (panel.total.clone(), panel.theme.clone());
        node!(|total| shown.set(*total.get()));
        node!(|theme| {
            let _ = theme.get();
            styled.update(bump);
        });
        lithia::flush().await;
        assert_eq!(lithia::test::live_nodes().await, base + 5);
    } // the panel drops
    // The effect keeps `total`, `theme` and `total`'s watcher; `theme`'s watcher only if it may write `theme`.
    assert_eq!(lithia::test::live_nodes().await, base + 4 + usize::from(theme_kept));
    count.set(2);
    lithia::flush().await;
    assert_eq!(*shown.get(), 2, "`total`'s watcher lives as long as the effect");
    drop(count);
    assert_eq!(lithia::test::live_nodes().await, base - 1);
}

#[lithia::test]
async fn case_ay_field_paths_are_precise() {
    panel_case(Themed::precise, false).await;
}

// Case AY: a method call on `self` uses it whole, so every handle in it may be written.
#[lithia::test]
async fn case_ay_self_used_whole() {
    panel_case(Themed::whole, true).await;
}

// Case AY: fields bound before the `node!` keep it precise.
#[lithia::test]
async fn case_ay_fields_bound_before() {
    panel_case(Themed::bound, false).await;
}

// Case AZ: an `Accessor` to a plain source node anchors nothing.
#[lithia::test]
async fn case_az_an_escaping_accessor_anchors_nothing() {
    let seen: SourceNode<u32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    let view: Accessor<u32> = {
        let local: SourceNode<u32> = node!(5);
        node!(|local| seen.set(*local.get()));
        lithia::flush().await;
        local.accessor()
    };
    assert_eq!((*seen.get(), *view.get()), (5, 5));
    assert_eq!(lithia::test::live_nodes().await, base + 1); // the watcher is freed; `local` is still readable
}

// Case BA: a scoped child kept by its run's value lives exactly as long as that value.
type Slot = Arc<Mutex<Option<DerivedNode<()>>>>;

#[lithia::test]
async fn case_ba_a_scoped_child_lives_as_long_as_the_value() {
    let tick: SourceNode<u32> = node!(0);
    let other: SourceNode<u32> = node!(0);
    let runs: SourceNode<u32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    let holder: DerivedNode<Slot> = node!(|tick| {
        let _ = tick.get();
        Arc::new(Mutex::new(Some(
            node!(|other| {
                let _ = other.get();
                runs.update(bump);
            })
            .scoped(),
        )))
    }); // the run releases the child at once
    lithia::flush().await;
    other.set(1);
    lithia::flush().await;
    assert_eq!((*runs.get(), lithia::test::live_nodes().await), (2, base + 2));
    let slot: Slot = (*holder.get()).clone();
    drop(slot.lock().unwrap().take());
    assert_eq!(lithia::test::live_nodes().await, base + 1); // the value let go of it, so it is gone
    other.set(2);
    lithia::flush().await;
    assert_eq!(*runs.get(), 2);
}

// Case BB: an effect owned by a struct field through `.scoped()`.
struct Widget {
    watch: DerivedNode<()>,
}

impl Widget {
    fn new(count: &SourceNode<u32>, runs: &SourceNode<u32>) -> Widget {
        let (count, runs): (SourceNode<u32>, SourceNode<u32>) = (count.clone(), runs.clone());
        Widget {
            watch: node!(|count| {
                let _ = count.get();
                runs.update(bump);
            })
            .scoped(),
        }
    }
}

#[lithia::test]
async fn case_bb_a_scoped_field() {
    let count: SourceNode<u32> = node!(0);
    let runs: SourceNode<u32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    let widget: Widget = Widget::new(&count, &runs);
    lithia::flush().await;
    count.set(1);
    lithia::flush().await;
    assert_eq!(*runs.get(), 2);
    drop(widget); // stops the effect
    count.set(2);
    lithia::flush().await;
    assert_eq!((*runs.get(), lithia::test::live_nodes().await), (2, base));
    let widget: Widget = Widget::new(&count, &runs);
    lithia::flush().await;
    let kept: DerivedNode<()> = widget.watch.clone();
    drop(widget);
    count.set(3);
    lithia::flush().await;
    assert_eq!(*runs.get(), 4); // the clone keeps it running
    assert!(matches!(lithia::status(&kept), Status::Ready), "no ghost");
    drop(kept);
    assert_eq!(lithia::test::live_nodes().await, base);
}

// Case BC: a plain source node and an async source block behave alike.
#[lithia::test]
async fn case_bc_both_kinds_of_source_alike() {
    let seen: SourceNode<u32> = node!(0);
    let tick: SourceNode<u32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    node!(1u32);
    node!(async { 1u32 });
    lithia::flush().await;
    assert_eq!(lithia::test::live_nodes().await, base, "discarded at the top level, both are freed");
    {
        let plain: SourceNode<u32> = node!(2);
        let block: SourceNode<u32> = node!(async { 3u32 });
        node!(|plain| seen.set(*plain.get()));
        node!(|block| seen.set(*block.get()));
        lithia::test::settled(&block).await;
        assert_eq!(lithia::test::live_nodes().await, base + 4);
    }
    assert_eq!(lithia::test::live_nodes().await, base, "their handles gone, both free their watchers");
    let creator: DerivedNode<()> = node!(|tick| {
        let _ = tick.get();
        node!(4u32);
        node!(async { 5u32 });
    });
    lithia::flush().await;
    assert_eq!(lithia::test::live_nodes().await, base + 3, "created in a body, both are held");
    tick.set(1);
    lithia::flush().await;
    assert_eq!(lithia::test::live_nodes().await, base + 3, "until the creator's next run is decided");
    lithia::dispose(&creator);
    assert_eq!(lithia::test::live_nodes().await, base);
}
