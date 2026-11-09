//! Support, captures and sweeps at their edges (spec §4.2, §10.1, §12).

#![allow(clippy::type_complexity, clippy::get_first)]

use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lithia::{DerivedNode, Readable, SourceNode, effect};
use tokio::sync::oneshot;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

/// Every event at WARN or TRACE from lithia, as `name=value` text.
fn events() -> Arc<Mutex<Vec<(Level, String)>>> {
    struct Fields(String);
    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            let _ = write!(self.0, "{}={:?} ", field.name(), value);
        }
        fn record_str(&mut self, field: &Field, value: &str) {
            let _ = write!(self.0, "{}={} ", field.name(), value);
        }
    }
    struct Collector(Arc<Mutex<Vec<(Level, String)>>>);
    impl<S: Subscriber> Layer<S> for Collector {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            let mut fields: Fields = Fields(String::new());
            event.record(&mut fields);
            self.0.lock().unwrap().push((*event.metadata().level(), fields.0));
        }
    }
    static EVENTS: OnceLock<Arc<Mutex<Vec<(Level, String)>>>> = OnceLock::new();
    EVENTS
        .get_or_init(|| {
            let events: Arc<Mutex<Vec<(Level, String)>>> = Arc::new(Mutex::new(Vec::new()));
            let subscriber: tracing_subscriber::layer::Layered<Collector, tracing_subscriber::Registry> =
                tracing_subscriber::registry().with(Collector(events.clone()));
            tracing::subscriber::set_global_default(subscriber).expect("one global subscriber");
            events
        })
        .clone()
}

/// Whether a warning containing `text` and naming `line` of this file arrives within `limit`.
async fn warned(text: &str, line: u32, limit: Duration) -> bool {
    let at: String = format!("support.rs:{line}:");
    let deadline: tokio::time::Instant = tokio::time::Instant::now() + limit;
    loop {
        let found: bool = events().lock().unwrap().iter().any(|(level, text_seen): &(Level, String)| {
            *level == Level::WARN && text_seen.contains(text) && text_seen.contains(&at)
        });
        if found || tokio::time::Instant::now() >= deadline {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn until(mut done: impl FnMut() -> bool) {
    while !done() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

fn bump(n: &u32) -> u32 {
    n + 1
}

/// Creates a handle-less watcher on `t`, lets `make` start a node on the long-lived `k` that
/// captures `t`, drops `t`'s handle, and reports whether the watcher is still alive: it is
/// exactly when the node may write `t`.
async fn keeps_watcher(make: impl FnOnce(&SourceNode<u32>, &SourceNode<u32>)) -> bool {
    let k: SourceNode<u32> = effect!(0);
    let seen: SourceNode<u32> = effect!(0);
    let base: usize = lithia::test::live_nodes().await;
    let t: SourceNode<u32> = effect!(1);
    effect!(|t| seen.set(*t.get()));
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
            effect!(|k| {
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
            effect!(|k| {
                let _ = k.get();
                pass_along(&t); // a function may write it
            });
        })
        .await,
        "passed to a function"
    );
    assert!(
        keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| {
            let (k, t): (SourceNode<u32>, SourceNode<u32>) = (k.clone(), t.clone());
            effect!(|k| {
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
            effect!(|k| println!("{:?} {:?}", k.get(), t.set(1)));
        })
        .await,
        "a write inside a formatting argument"
    );
    assert!(
        keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| {
            let (k, items): (SourceNode<u32>, Vec<SourceNode<u32>>) = (k.clone(), vec![t.clone()]);
            effect!(|k| {
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
    effect!(|k| {
        let _ = (k.get(), n.get());
    });
}

/// A generic dependency only read is a read-only capture.
fn generic_dependency<R: Readable<Value = u32> + Clone + Send + Sync + 'static>(k: &SourceNode<u32>, n: &R) {
    let (k, n): (SourceNode<u32>, R) = (k.clone(), n.clone());
    effect!(|k, n| {
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
            effect!(|k| {
                let _ = (k.get(), pair.t.get());
            });
        })
        .await,
        "a field path only read"
    );
    assert!(
        keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| {
            let (k, pair): (SourceNode<u32>, Pair) = (k.clone(), Pair { t: t.clone(), label: "p".to_string() });
            effect!(|k| {
                let _ = (k.get(), pair.t.get(), pair.label.len()); // `label` is not a handle
            });
        })
        .await,
        "a field path that is not a handle"
    );
    assert!(
        keeps_watcher(|k: &SourceNode<u32>, t: &SourceNode<u32>| {
            let (k, pair): (SourceNode<u32>, Pair) = (k.clone(), Pair { t: t.clone(), label: "p".to_string() });
            effect!(|k| {
                let _ = k.get();
                let held: &Pair = &pair; // a use of the name itself
                let _ = held.t.get();
            });
        })
        .await,
        "the name used whole"
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
    let k: SourceNode<u32> = effect!(0);
    let counter: DerivedNode<()> = effect!(|k| {
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
    let k: SourceNode<u32> = effect!(0);
    let seen: SourceNode<u32> = effect!(0);
    let slot: Arc<Mutex<Option<SourceNode<u32>>>> = Arc::new(Mutex::new(None));
    let base: usize = lithia::test::live_nodes().await;
    let t: SourceNode<u32> = effect!(4);
    effect!(|t| seen.set(*t.get()));
    let copier: DerivedNode<()> = effect!(|k| {
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
async fn a_handle_moved_out_of_an_async_move_block_is_not_counted() {
    let seen: SourceNode<u32> = effect!(0);
    let base: usize = lithia::test::live_nodes().await;
    let (tx, rx): (oneshot::Sender<SourceNode<u32>>, oneshot::Receiver<SourceNode<u32>>) = oneshot::channel();
    let t: SourceNode<u32> = effect!(3);
    let block: SourceNode<()> = effect!(async move {
        let _ = tx.send(t);
    });
    let moved: SourceNode<u32> = rx.await.unwrap(); // an internal handle, out of the block
    let watcher: DerivedNode<()> = effect!(|moved| seen.set(*moved.get()));
    lithia::flush().await;
    assert_eq!(*seen.get(), 3);
    drop(block);
    drop(watcher); // `moved` anchors nothing, so the watcher goes with its own handle
    assert_eq!(lithia::test::live_nodes().await, base + 1);
    drop(moved);
    assert_eq!(lithia::test::live_nodes().await, base);
}

#[lithia::test]
async fn scoped_on_a_node_support_let_go_of() {
    let base: usize = lithia::test::live_nodes().await;
    let x: SourceNode<u32> = effect!(1);
    let m: DerivedNode<u32> = effect!(|x| *x.get() * 2);
    let (tx, rx): (oneshot::Sender<DerivedNode<u32>>, oneshot::Receiver<DerivedNode<u32>>) = oneshot::channel();
    let block: SourceNode<()> = effect!(async move {
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
    let x: SourceNode<u32> = effect!(0);
    let seen: SourceNode<u32> = effect!(0);
    let base: usize = lithia::test::live_nodes().await;
    let t: SourceNode<u32> = effect!(1);
    let watch_x: DerivedNode<()> = effect!(|x| {
        let _ = x.get();
    });
    effect!(|t| seen.set(*t.get()));
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
    let s: SourceNode<u32> = effect!(0);
    let trigger: SourceNode<u32> = effect!(0);
    let hub: DerivedNode<u32> = effect!(|s| *s.get());
    for _ in 0..20_000 {
        effect!(|hub| {
            let _ = hub.get();
        });
    }
    fn helper(n: &lithia::Accessor<u32>) -> u32 {
        *n.get()
    }
    effect!(|trigger| {
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
        .filter(|(level, text): &&(Level, String)| *level == Level::TRACE && text.contains("lithia: sweep"))
        .map(|(_, text): &(Level, String)| {
            let at: usize = text.find("walked=").unwrap() + "walked=".len();
            text[at..].split(' ').next().unwrap().parse::<usize>().unwrap()
        })
        .sum();
    // Walking the hub's downstream would take 20 000 nodes per drop; other tests' sweeps add a few.
    assert!(walked < 20_000, "sweeps walked {walked} nodes");
}

#[lithia::test]
async fn blocking_work_creating_after_its_node_was_freed_warns() {
    let _ = events();
    let (go_tx, go_rx): (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>) = std::sync::mpsc::channel();
    let go_rx: Arc<Mutex<std::sync::mpsc::Receiver<()>>> = Arc::new(Mutex::new(go_rx));
    let created: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let other: SourceNode<u32> = effect!(0);
    let line: u32 = line!() + 9;
    {
        let k: SourceNode<u32> = effect!(0);
        effect!(|k| {
            let _ = k.get();
            let (go_rx, created, other): (Arc<Mutex<std::sync::mpsc::Receiver<()>>>, Arc<AtomicBool>, SourceNode<u32>) =
                (go_rx.clone(), created.clone(), other.clone());
            lithia::spawn_blocking(move || {
                let _ = go_rx.lock().unwrap().recv();
                effect!(|other| {
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
    let k: SourceNode<u32> = effect!(0);
    let line: u32 = line!() + 2;
    for _ in 0..1_000 {
        effect!(|k| {
            let _ = k.get();
        });
    }
    assert!(warned("keep piling up", line, Duration::from_secs(2)).await);
    lithia::dispose(&k);
}

#[lithia::test]
async fn live_nodes_drains_the_drops_it_raised() {
    let rows: SourceNode<Vec<DerivedNode<()>>> = effect!(Vec::new());
    let n: SourceNode<u32> = effect!(0);
    let base: usize = lithia::test::live_nodes().await;
    rows.set(
        (0..20)
            .map(|_| {
                let n: SourceNode<u32> = n.clone();
                effect!(|n| {
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
    let seen: SourceNode<u32> = effect!(0);
    for _ in 0..200 {
        let n: SourceNode<u32> = effect!(1);
        let other: SourceNode<u32> = n.clone();
        let seen_there: SourceNode<u32> = seen.clone();
        let runtime: tokio::runtime::Handle = tokio::runtime::Handle::current();
        let creator: std::thread::JoinHandle<()> = std::thread::spawn(move || {
            let _entered: tokio::runtime::EnterGuard<'_> = runtime.enter();
            let made: DerivedNode<()> = effect!(|other| seen_there.update(bump));
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
