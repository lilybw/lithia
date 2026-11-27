// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Support, captures and sweeps at their edges (spec §4.2, §10.1, §12).

#![allow(clippy::type_complexity, clippy::get_first)]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lithia::{DerivedNode, Readable, Setter, SourceNode, node};
use tokio::sync::oneshot;
use tracing::Level;

use lithia_test::{Captured, events, until};

/// Whether a warning containing `text` and naming `line` of this file arrives within `limit`.
async fn warned(text: &str, line: u32, limit: Duration) -> bool {
    lithia_test::warned("support.rs", text, line, limit).await
}

fn bump(n: &u32) -> u32 {
    n + 1
}

/// Whether a handle-less watcher on `t` outlives `t`'s handle beside the node `make` starts on `k`
/// capturing `t`: exactly when that node may write `t`.
async fn keeps_watcher(make: impl FnOnce(&SourceNode<u32>, &SourceNode<u32>)) -> bool {
    let k: SourceNode<u32> = node!(0);
    let seen: SourceNode<u32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    let t: SourceNode<u32> = node!(1);
    node!(|t| seen.set(*t.get()));
    make(&k, &t);
    lithia::flush().await;
    drop(t);
    let live: usize = lithia::test::live_nodes().await;
    assert!(live == base + 2 || live == base + 3, "live {live}, base {base}");
    lithia::dispose(&k);
    live == base + 3
}

fn pass_along(n: &SourceNode<u32>) {
    let _ = n.get();
}

macro_rules! touch {
    ($n:ident) => {
        let _ = $n.get();
    };
}

#[lithia::test]
async fn reads_make_no_edge_and_other_uses_do() {
    assert!(
        !keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| {
            let (k, t): (SourceNode<u32>, SourceNode<u32>) = (k.clone(), t.clone());
            node!(|k| {
                let _ = (k.get(), t.get(), t.try_get(), t.accessor(), lithia::status(&t));
                println!("{t:?} {:?}", t.get());
            });
        })
        .await,
        "reads"
    );
    assert!(
        keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| {
            let (k, t): (SourceNode<u32>, SourceNode<u32>) = (k.clone(), t.clone());
            node!(|k| {
                let _ = k.get();
                pass_along(t); // a function may write it
            });
        })
        .await,
        "passed to a function"
    );
    assert!(
        keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| {
            let (k, t): (SourceNode<u32>, SourceNode<u32>) = (k.clone(), t.clone());
            node!(|k| {
                let _ = k.get();
                touch!(t); // a user macro may write it
            });
        })
        .await,
        "a user macro"
    );
    assert!(
        keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| {
            let (k, t): (SourceNode<u32>, SourceNode<u32>) = (k.clone(), t.clone());
            node!(|k| println!("{:?} {:?}", k.get(), t.set(1)));
        })
        .await,
        "a write inside a formatting argument"
    );
    assert!(
        keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| {
            let (k, items): (SourceNode<u32>, Vec<SourceNode<u32>>) = (k.clone(), vec![t.clone()]);
            node!(|k| {
                let _ = k.get();
                let _ = items.get(0).map(|item: &SourceNode<u32>| *item.get()); // not a handle's `get`
            });
        })
        .await,
        "`items.get(0)` on a captured `Vec`"
    );
}

/// A generic helper: `n` is not a dependency, so its capture may write.
fn generic_reader<R: Readable<Value = u32> + Clone + Send + Sync + 'static>(k: &SourceNode<u32>, n: &R) {
    let (k, n): (SourceNode<u32>, R) = (k.clone(), n.clone());
    node!(|k| {
        let _ = (k.get(), n.get());
    });
}

/// A generic dependency only read is a read-only capture.
fn generic_dependency<R: Readable<Value = u32> + Clone + Send + Sync + 'static>(k: &SourceNode<u32>, n: &R) {
    let (k, n): (SourceNode<u32>, R) = (k.clone(), n.clone());
    node!(|k, n| {
        let _ = (k.get(), n.get());
    });
}

#[derive(Clone)]
struct Pair {
    t: SourceNode<u32>,
    label: String,
}

#[lithia::test]
async fn generic_code_self_and_field_paths() {
    assert!(
        keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| generic_reader(k, t)).await,
        "generic non-dependency"
    );
    assert!(
        !keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| generic_dependency(k, t)).await,
        "generic dependency"
    );
    assert!(
        !keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| {
            let (k, pair): (SourceNode<u32>, Pair) = (k.clone(), Pair { t: t.clone(), label: "p".to_string() });
            node!(|k| {
                let _ = (k.get(), pair.t.get());
            });
        })
        .await,
        "a field path only read"
    );
    assert!(
        keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| {
            let (k, pair): (SourceNode<u32>, Pair) = (k.clone(), Pair { t: t.clone(), label: "p".to_string() });
            node!(|k| {
                let _ = (k.get(), pair.t.get(), pair.label.len()); // `label` is not a handle
            });
        })
        .await,
        "a field path that is not a handle"
    );
    assert!(
        keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| {
            let (k, pair): (SourceNode<u32>, Pair) = (k.clone(), Pair { t: t.clone(), label: "p".to_string() });
            node!(|k| {
                let _ = k.get();
                let held: &Pair = pair; // a use of the name itself
                let _ = held.t.get();
            });
        })
        .await,
        "the name used whole"
    );
}

#[lithia::test]
async fn move_bodies_classify_field_paths_too() {
    assert!(
        !keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| {
            let (k, pair): (SourceNode<u32>, Pair) = (k.clone(), Pair { t: t.clone(), label: "p".to_string() });
            node!(move |k| {
                let _ = (k.get(), pair.t.get());
            });
        })
        .await,
        "a field path only read"
    );
    assert!(
        keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| {
            let (k, pair): (SourceNode<u32>, Pair) = (k.clone(), Pair { t: t.clone(), label: "p".to_string() });
            node!(move |k| {
                let _ = k.get();
                pair.t.set(1);
            });
        })
        .await,
        "a field path written"
    );
    assert!(
        keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| {
            let (k, pair): (SourceNode<u32>, Pair) = (k.clone(), Pair { t: t.clone(), label: "p".to_string() });
            node!(move |k| {
                let _ = (k.get(), pair.t.get(), pair.label.len()); // `label` is not a handle
            });
        })
        .await,
        "a field path that is not a handle"
    );
}

struct Guard(Arc<AtomicBool>);

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

struct AppState {
    hits: AtomicU32,
    _guard: Guard,
}

#[lithia::test]
async fn a_field_through_an_arc_compiles_and_its_guard_lives_with_the_effect() {
    let dropped: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let state: Arc<AppState> = Arc::new(AppState { hits: AtomicU32::new(0), _guard: Guard(dropped.clone()) });
    let k: SourceNode<u32> = node!(0);
    let counter: DerivedNode<()> = node!(|k| {
        let _ = k.get();
        state.hits.fetch_add(1, Ordering::SeqCst);
    });
    let probe: Arc<AppState> = state.clone();
    drop(state);
    lithia::flush().await;
    k.set(1);
    lithia::flush().await;
    assert_eq!(probe.hits.load(Ordering::SeqCst), 2);
    drop(probe);
    assert!(!dropped.load(Ordering::SeqCst), "the effect's capture keeps the guard");
    lithia::dispose(&counter);
    until(|| dropped.load(Ordering::SeqCst)).await;
}

#[lithia::test]
async fn handles_cloned_out_of_a_body_are_outside_handles() {
    let k: SourceNode<u32> = node!(0);
    let seen: SourceNode<u32> = node!(0);
    let slot: Arc<Mutex<Option<SourceNode<u32>>>> = Arc::new(Mutex::new(None));
    let base: usize = lithia::test::live_nodes().await;
    let t: SourceNode<u32> = node!(4);
    node!(|t| seen.set(*t.get()));
    let copier: DerivedNode<()> = node!(|k| {
        let _ = k.get();
        *slot.lock().unwrap() = Some(t.clone());
    });
    lithia::flush().await;
    drop(t);
    lithia::dispose(&copier);
    // The copy in the slot anchors `t`, so its watcher lives on after the copier is gone.
    assert_eq!(lithia::test::live_nodes().await, base + 2);
    slot.lock().unwrap().take();
    assert_eq!(lithia::test::live_nodes().await, base);
}

#[lithia::test]
async fn a_handle_moved_out_of_an_async_move_block_counts_once_the_block_ends() {
    let seen: SourceNode<u32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    let (tx, rx): (oneshot::Sender<SourceNode<u32>>, oneshot::Receiver<SourceNode<u32>>) = oneshot::channel();
    let t: SourceNode<u32> = node!(3);
    let block: SourceNode<()> = node!(async move {
        let _ = tx.send(t);
    });
    let moved: SourceNode<u32> = rx.await.unwrap(); // out of the block, which has ended
    let watcher: DerivedNode<()> = node!(|moved| seen.set(*moved.get()));
    lithia::flush().await;
    assert_eq!(*seen.get(), 3);
    drop(block);
    drop(watcher); // `moved` anchors `t` now, so support keeps the watcher
    assert_eq!(lithia::test::live_nodes().await, base + 2);
    moved.set(4);
    lithia::flush().await;
    assert_eq!(*seen.get(), 4);
    drop(moved);
    assert_eq!(lithia::test::live_nodes().await, base);
}

#[lithia::test]
async fn a_handle_moved_out_of_a_finished_move_block_anchors_a_scoped_node() {
    let seen: SourceNode<u32> = node!(0);
    let (tx, rx): (oneshot::Sender<SourceNode<u32>>, oneshot::Receiver<SourceNode<u32>>) = oneshot::channel();
    // Scoped: only its own handles support it, not the block that writes it.
    let t: SourceNode<u32> = node!(3).scoped();
    let block: SourceNode<()> = node!(async move {
        let _ = tx.send(t);
    });
    let back: SourceNode<u32> = rx.await.unwrap();
    lithia::test::settled(&block).await;
    node!(|back| seen.set(*back.get()));
    lithia::flush().await;
    // A sweep starts at `t` if its anchor count crosses zero; `back` keeps it above.
    drop(back.clone());
    back.set(5);
    lithia::flush().await;
    assert_eq!(*seen.get(), 5);
    drop(block);
}

// One thread, so the block's run task cannot end the lease before disposal does.
#[lithia::test(flavor = "current_thread")]
async fn a_handle_moved_out_of_a_disposed_move_block_counts_at_once() {
    let seen: SourceNode<u32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    let (tx, rx): (oneshot::Sender<SourceNode<u32>>, oneshot::Receiver<SourceNode<u32>>) = oneshot::channel();
    let t: SourceNode<u32> = node!(3);
    let block: SourceNode<()> = node!(async move {
        let _ = tx.send(t);
        std::future::pending::<()>().await;
    });
    let moved: SourceNode<u32> = rx.await.unwrap(); // out of the block, which still runs
    node!(|moved| seen.set(*moved.get()));
    lithia::flush().await;
    // Disposal ends the block's lease before support is recomputed, so `moved` anchors `t` in time.
    lithia::dispose(&block);
    moved.set(5);
    lithia::flush().await;
    assert_eq!(*seen.get(), 5);
    drop(block);
    assert_eq!(lithia::test::live_nodes().await, base + 2);
    drop(moved);
    assert_eq!(lithia::test::live_nodes().await, base);
}

#[lithia::test]
async fn an_anchor_that_returns_holds_again_what_support_let_go_of() {
    let go: SourceNode<u32> = node!(0);
    let slot: Arc<Mutex<Option<lithia::Accessor<u32>>>> = Arc::new(Mutex::new(None));
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let base: usize = lithia::test::live_nodes().await;
    let p: SourceNode<u32> = node!(1);
    let d: DerivedNode<u32> = node!(|p| *p.get());
    let e: DerivedNode<()> = node!(|d| {
        let _ = d.get();
        runs.fetch_add(1, Ordering::SeqCst);
    });
    // `w` keeps `d` and `e` alive through what its body reads, and can hand out `d` again;
    // scoped, it goes with its handle.
    let w: DerivedNode<()> = node!(|go| {
        if *go.get() > 0 {
            *slot.lock().unwrap() = Some(d.accessor());
        }
        let _ = e.get();
    })
    .scoped();
    lithia::flush().await;
    drop((e, d, p)); // nothing anchors `p` or `d` now: support lets go of `d` and `e`
    assert_eq!(lithia::test::live_nodes().await, base + 4);
    go.set(1); // `d` is an anchor again
    lithia::flush().await;
    drop(w); // what kept `d` and `e` alive goes; support holds them again
    assert_eq!(lithia::test::live_nodes().await, base + 3);
    let reader: lithia::Accessor<u32> = slot.lock().unwrap().take().unwrap();
    assert_eq!((*reader.get(), runs.load(Ordering::SeqCst)), (1, 1));
    drop(reader);
    assert_eq!(lithia::test::live_nodes().await, base);
}

#[lithia::test]
async fn a_new_writer_holds_again_what_support_let_go_of() {
    let base: usize = lithia::test::live_nodes().await;
    let go: SourceNode<u32> = node!(2);
    let x: SourceNode<u32> = node!(0);
    let seen: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let e: DerivedNode<()> = node!(|x| seen.store(*x.get(), Ordering::SeqCst));
    let (tx, rx): (oneshot::Sender<()>, oneshot::Receiver<()>) = oneshot::channel();
    let parent: SourceNode<u32> = go.clone();
    // Holds `x` only inside its body; once told to, its run creates a writer of `x` on `go`.
    let block: SourceNode<()> = node!(async move {
        let _ = rx.await;
        node!(|parent| x.set(*parent.get() * 10));
        std::future::pending::<()>().await;
    });
    // Scoped, so each lives by its handle; their bodies keep `block` and `e` alive.
    let keep_block: DerivedNode<()> = node!(|go| {
        let _ = (go.get(), block.get());
    })
    .scoped();
    let keep_e: DerivedNode<()> = node!(|go| {
        let _ = (go.get(), e.get());
    })
    .scoped();
    lithia::flush().await;
    drop((e, block)); // nothing anchors `x` or its writer: support lets go of `e`
    assert_eq!(lithia::test::live_nodes().await, base + 6);
    tx.send(()).unwrap();
    until(|| seen.load(Ordering::SeqCst) == 20).await; // the block's run goes on outside any flush
    assert_eq!(lithia::test::live_nodes().await, base + 7);
    drop(keep_e); // the new writer, on the anchored `go`, keeps `e`
    assert_eq!(lithia::test::live_nodes().await, base + 6);
    go.set(3);
    lithia::flush().await;
    assert_eq!(seen.load(Ordering::SeqCst), 30);
    drop(keep_block);
    assert_eq!(lithia::test::live_nodes().await, base + 1);
}

#[lithia::test]
async fn a_scoped_node_that_is_an_anchor_again_holds_what_depends_on_it() {
    let base: usize = lithia::test::live_nodes().await;
    let a: SourceNode<u32> = node!(1);
    let s: DerivedNode<u32> = node!(|a| *a.get()).scoped();
    let seen: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let d: DerivedNode<()> = node!(|s| seen.store(*s.get(), Ordering::SeqCst));
    let slot: Arc<Mutex<Option<DerivedNode<u32>>>> = Arc::new(Mutex::new(None));
    // Its body keeps `d` alive, and hands out `s` once `a` passes 1; scoped, it goes with its handle.
    let w: DerivedNode<()> = node!(|a| {
        if *a.get() > 1 {
            *slot.lock().unwrap() = Some(s.clone());
        }
        let _ = d.get();
    })
    .scoped();
    lithia::flush().await;
    drop((d, s)); // `s` has no handle now, so support lets go of `d`
    assert_eq!(lithia::test::live_nodes().await, base + 4);
    a.set(2); // `s` is an anchor again
    lithia::flush().await;
    drop(w); // what kept `d` alive goes; support holds it again
    assert_eq!(lithia::test::live_nodes().await, base + 3);
    a.set(3);
    lithia::flush().await;
    assert_eq!(seen.load(Ordering::SeqCst), 3);
    slot.lock().unwrap().take();
    assert_eq!(lithia::test::live_nodes().await, base + 1);
}

#[lithia::test]
async fn scoped_on_a_node_support_let_go_of() {
    let base: usize = lithia::test::live_nodes().await;
    let x: SourceNode<u32> = node!(1);
    let m: DerivedNode<u32> = node!(|x| *x.get() * 2);
    let (tx, rx): (oneshot::Sender<DerivedNode<u32>>, oneshot::Receiver<DerivedNode<u32>>) = oneshot::channel();
    let block: SourceNode<()> = node!(async move {
        let _ = tx.send(m);
    });
    let m: DerivedNode<u32> = rx.await.unwrap(); // internal, so `m` anchors nothing
    drop(block);
    lithia::flush().await;
    drop(x); // support lets go of `m`; the internal handle still holds it
    assert_eq!(lithia::test::live_nodes().await, base + 2);
    let m: DerivedNode<u32> = m.scoped();
    assert_eq!(*m.get(), 2);
    drop(m);
    assert_eq!(lithia::test::live_nodes().await, base);
}

#[lithia::test]
async fn a_sweep_inside_an_update_closure() {
    let x: SourceNode<u32> = node!(0);
    let seen: SourceNode<u32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    let t: SourceNode<u32> = node!(1);
    let watch_x: DerivedNode<()> = node!(|x| {
        let _ = x.get();
    });
    node!(|t| seen.set(*t.get()));
    lithia::flush().await;
    let held: Mutex<Option<(SourceNode<u32>, DerivedNode<()>)>> = Mutex::new(Some((t, watch_x)));
    x.update(|v: &u32| {
        drop(held.lock().unwrap().take()); // last handles drop with `x`'s write lock held
        v + 1
    });
    lithia::flush().await;
    assert_eq!((*x.get(), lithia::test::live_nodes().await), (1, base + 1)); // only `watch_x` lives, on `x`
}

#[lithia::test]
async fn a_supported_starting_point_ends_its_sweep_at_once() {
    let _ = events();
    let s: SourceNode<u32> = node!(0);
    let trigger: SourceNode<u32> = node!(0);
    let hub: DerivedNode<u32> = node!(|s| *s.get());
    let children: usize = if cfg!(miri) { 2_000 } else { 20_000 }; // 20 000 take Miri hours
    for _ in 0..children {
        node!(|hub| {
            let _ = hub.get();
        });
    }
    fn helper(n: &lithia::Accessor<u32>) -> u32 {
        *n.get()
    }
    node!(|trigger| {
        let _ = trigger.get();
        helper(&hub.accessor()) // a temporary anchor on a supported node
    });
    drop(hub);
    lithia::flush().await;
    let before: usize = events().lock().unwrap().len();
    for value in 1..=50u32 {
        trigger.set(value);
        lithia::flush().await;
    }
    let walked: usize = events().lock().unwrap()[before..]
        .iter()
        .filter(|event: &&Captured| event.level == Level::TRACE && event.text.contains("lithia: sweep"))
        .map(|event: &Captured| {
            let at: usize = event.text.find("walked=").unwrap() + "walked=".len();
            event.text[at..].split(' ').next().unwrap().parse::<usize>().unwrap()
        })
        .sum();
    // Walking the hub's downstream would take `children` nodes per drop; other tests' sweeps add a few.
    assert!(walked < children, "sweeps walked {walked} nodes");
}

#[lithia::test]
async fn blocking_work_creating_after_its_node_was_freed_warns() {
    let _ = events();
    let (go_tx, go_rx): (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>) = std::sync::mpsc::channel();
    let go_rx: Arc<Mutex<std::sync::mpsc::Receiver<()>>> = Arc::new(Mutex::new(go_rx));
    let created: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let other: SourceNode<u32> = node!(0);
    let line: u32 = line!() + 9;
    {
        let k: SourceNode<u32> = node!(0);
        node!(|k| {
            let _ = k.get();
            let (go_rx, created, other): (Arc<Mutex<std::sync::mpsc::Receiver<()>>>, Arc<AtomicBool>, SourceNode<u32>) =
                (go_rx.clone(), created.clone(), other.clone());
            lithia::spawn_blocking(move || {
                let _ = go_rx.lock().unwrap().recv();
                node!(|other| {
                    let _ = other.get();
                });
                created.store(true, Ordering::SeqCst);
            });
        });
        lithia::flush().await;
    } // `k` goes, and the effect with it, while its blocking work waits
    go_tx.send(()).unwrap();
    until(|| created.load(Ordering::SeqCst)).await;
    assert!(warned("freed before its first run", line, Duration::from_secs(2)).await);
}

#[lithia::test]
async fn the_growth_warning_names_the_site() {
    let _ = events();
    let k: SourceNode<u32> = node!(0);
    let line: u32 = line!() + 2;
    for _ in 0..1_000 {
        node!(|k| {
            let _ = k.get();
        });
    }
    assert!(warned("keep piling up", line, Duration::from_secs(2)).await);
    lithia::dispose(&k);
}

#[lithia::test]
async fn live_nodes_drains_the_drops_it_raised() {
    let rows: SourceNode<Vec<DerivedNode<()>>> = node!(Vec::new());
    let n: SourceNode<u32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    rows.set(
        (0..20)
            .map(|_| {
                let n: SourceNode<u32> = n.clone();
                node!(|n| {
                    let _ = n.get();
                })
                .scoped()
            })
            .collect(),
    );
    lithia::flush().await;
    assert_eq!(lithia::test::live_nodes().await, base + 20);
    rows.set(Vec::new()); // the old value drops on the blocking pool, and its nodes with it
    assert_eq!(lithia::test::live_nodes().await, base);
}

#[lithia::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_last_handle_drop_races_a_creation_on_the_same_node() {
    let base: usize = lithia::test::live_nodes().await;
    let seen: SourceNode<u32> = node!(0);
    for _ in 0..200 {
        let n: SourceNode<u32> = node!(1);
        let other: SourceNode<u32> = n.clone();
        let seen_there: SourceNode<u32> = seen.clone();
        let runtime: tokio::runtime::Handle = tokio::runtime::Handle::current();
        let creator: std::thread::JoinHandle<()> = std::thread::spawn(move || {
            let _entered: tokio::runtime::EnterGuard<'_> = runtime.enter();
            let made: DerivedNode<()> = node!(|other| seen_there.update(bump));
            drop(other);
            drop(made);
        });
        drop(n);
        creator.join().unwrap();
    }
    lithia::flush().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(lithia::test::live_nodes().await, base + 1); // only `seen`
}

#[lithia::test]
async fn a_setter_anchors_what_its_node_feeds_until_it_drops() {
    let seen: SourceNode<u32> = node!(0);
    let base: usize = lithia::test::live_nodes().await;
    let t: SourceNode<u32> = node!(1);
    node!(|t| seen.set(*t.get()));
    let writer: Setter<u32> = t.setter();
    let debug: String = format!("{writer:?}");
    assert!(debug.starts_with("Setter { id: ") && debug.contains("support.rs:"), "{debug}");
    drop(t);
    assert_eq!(lithia::test::live_nodes().await, base + 2, "the setter can still change `t`");
    writer.set(5);
    lithia::flush().await;
    assert_eq!(*seen.get(), 5);
    let copy: Setter<u32> = writer.clone();
    drop(writer);
    assert_eq!(lithia::test::live_nodes().await, base + 2, "its clone anchors as well");
    drop(copy);
    assert_eq!(lithia::test::live_nodes().await, base, "with the last setter gone, nothing can change `t`");
}

#[derive(Clone)]
struct Outlet {
    w: Setter<u32>,
    label: String,
}

#[lithia::test]
async fn a_setter_reached_by_a_field_path_may_write() {
    assert!(
        keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| {
            let (k, out): (SourceNode<u32>, Outlet) = (k.clone(), Outlet { w: t.setter(), label: "o".to_string() });
            node!(|k| {
                let _ = k.get();
                out.w.set(1);
            });
        })
        .await,
        "a field path"
    );
    assert!(
        keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| {
            let (k, out): (SourceNode<u32>, Outlet) = (k.clone(), Outlet { w: t.setter(), label: "o".to_string() });
            node!(move |k| {
                let _ = (k.get(), out.label.len());
                out.w.set(1);
            });
        })
        .await,
        "a field path in a move body"
    );
}

#[lithia::test]
async fn a_static_reference_as_a_moved_dependency_leaves_its_handle_outside() {
    let seen: SourceNode<u32> = node!(0);
    let t: SourceNode<u32> = node!(1);
    let leaked: &'static SourceNode<u32> = Box::leak(Box::new(t.clone()));
    node!(|t| seen.set(*t.get()));
    drop(t);
    // The body moves the reference, not the handle it points to, which stays outside.
    let child: DerivedNode<u32> = node!(move |leaked| *leaked.get() + 1);
    lithia::flush().await;
    assert_eq!(*child.get(), 2);
    let _ = lithia::test::live_nodes().await;
    leaked.set(7);
    lithia::flush().await;
    assert_eq!((*seen.get(), *child.get()), (7, 8), "the handle outside still reaches every reader");
}
