// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Validation cases BD–BK, added by v0.3.1: keyed nodes reclaimed across a creator's runs (spec §10.1, §13).
//! Every case runs under the workspace's test timeout (`.cargo/config.toml`).

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lithia::{DerivedNode, SourceNode, effect};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

#[derive(Clone, PartialEq, Debug)]
struct Row {
    id: u32,
    title: String,
}

fn row(id: u32, title: &str) -> Row {
    Row { id, title: title.to_string() }
}

/// The level and text of every event emitted in this binary.
type Events = Arc<Mutex<Vec<(Level, String)>>>;

/// The events collected so far, collecting from the first call on.
fn events() -> Events {
    struct Fields(String);
    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            let _ = write!(self.0, "{}={:?} ", field.name(), value);
        }
        fn record_str(&mut self, field: &Field, value: &str) {
            let _ = write!(self.0, "{}={} ", field.name(), value);
        }
    }
    struct Collector(Events);
    impl<S: Subscriber> Layer<S> for Collector {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            let mut fields: Fields = Fields(String::new());
            event.record(&mut fields);
            self.0.lock().unwrap().push((*event.metadata().level(), fields.0));
        }
    }
    static EVENTS: OnceLock<Events> = OnceLock::new();
    EVENTS
        .get_or_init(|| {
            let events: Events = Arc::new(Mutex::new(Vec::new()));
            let subscriber: tracing_subscriber::layer::Layered<Collector, tracing_subscriber::Registry> =
                tracing_subscriber::registry().with(Collector(events.clone()));
            tracing::subscriber::set_global_default(subscriber).expect("one global subscriber");
            events
        })
        .clone()
}

/// Whether an event at `level` containing `text` and naming a site in this file has arrived.
fn reported(level: Level, text: &str) -> bool {
    events()
        .lock()
        .unwrap()
        .iter()
        .any(|(at, event): &(Level, String)| *at == level && event.contains(text) && event.contains("keyed.rs:"))
}

fn texts(list: &DerivedNode<Vec<DerivedNode<String>>>) -> Vec<String> {
    list.get().iter().map(|node: &DerivedNode<String>| (*node.get()).clone()).collect()
}

fn values(list: &DerivedNode<Vec<DerivedNode<String>>>) -> Vec<Arc<String>> {
    list.get().iter().map(|node: &DerivedNode<String>| node.get()).collect()
}

// Case BD: rows keep their nodes by key; only a changed or new row runs, a removed one is freed.
#[lithia::test]
async fn case_bd_rows_keep_their_nodes() {
    let rows: SourceNode<Vec<Row>> = effect!(vec![row(1, "a"), row(2, "b")]);
    let suffix: SourceNode<&'static str> = effect!("!");
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let base: usize = lithia::test::live_nodes().await;
    let list: DerivedNode<Vec<DerivedNode<String>>> = effect!(|rows| {
        rows.get()
            .iter()
            .map(|row: &Row| {
                effect!(
                    |suffix| {
                        runs.fetch_add(1, Ordering::SeqCst);
                        format!("{}{}", row.title, suffix.get())
                    },
                    key = row.id
                )
            })
            .collect::<Vec<DerivedNode<String>>>()
    });
    lithia::flush().await;
    assert_eq!(texts(&list), ["a!", "b!"]);
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    let before: Vec<Arc<String>> = values(&list);

    // Reordered: both rows are reclaimed with equal captures, so neither runs.
    rows.set(vec![row(2, "b"), row(1, "a")]);
    lithia::flush().await;
    assert_eq!(texts(&list), ["b!", "a!"]);
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    let after: Vec<Arc<String>> = values(&list);
    assert!(Arc::ptr_eq(&before[0], &after[1]) && Arc::ptr_eq(&before[1], &after[0]));

    // A changed row reruns in place, a new row is created.
    rows.set(vec![row(2, "B"), row(1, "a"), row(3, "c")]);
    lithia::flush().await;
    assert_eq!(texts(&list), ["B!", "a!", "c!"]);
    assert_eq!(runs.load(Ordering::SeqCst), 4);
    assert_eq!(lithia::test::live_nodes().await, base + 4);

    // Removed rows are let go of when the list's run ends; the one left reacts alone.
    rows.set(vec![row(3, "c")]);
    lithia::flush().await;
    assert_eq!(texts(&list), ["c!"]);
    assert_eq!(lithia::test::live_nodes().await, base + 2);
    suffix.set("?");
    lithia::flush().await;
    assert_eq!(texts(&list), ["c?"]);
    assert_eq!(runs.load(Ordering::SeqCst), 5);
}

// Case BE: a keyed source node takes the list's value when it differs, and keeps its watchers.
#[lithia::test]
async fn case_be_keyed_source_nodes_take_new_values() {
    let rows: SourceNode<Vec<Row>> = effect!(vec![row(1, "a"), row(2, "b")]);
    let items: DerivedNode<Vec<SourceNode<String>>> = effect!(|rows| {
        rows.get()
            .iter()
            .map(|row: &Row| effect!(row.title.clone(), key = |row| row.id))
            .collect::<Vec<SourceNode<String>>>()
    });
    lithia::flush().await;
    let first: SourceNode<String> = items.get()[0].clone();
    let seen: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let _watch: DerivedNode<()> = effect!(|first| {
        let _ = first.get();
        seen.fetch_add(1, Ordering::SeqCst);
    });
    lithia::flush().await;
    assert_eq!(seen.load(Ordering::SeqCst), 1);

    // Reordered with equal values: the same node, not written.
    rows.set(vec![row(2, "b"), row(1, "a")]);
    lithia::flush().await;
    assert_eq!(*items.get()[1].get(), "a");
    assert_eq!(seen.load(Ordering::SeqCst), 1);

    // A new title is written to the node the watcher already follows.
    rows.set(vec![row(2, "b"), row(1, "A")]);
    lithia::flush().await;
    assert_eq!(*first.get(), "A");
    assert_eq!(seen.load(Ordering::SeqCst), 2);

    // A write from elsewhere is replaced by the list's value at the creator's next run.
    first.set("x".to_string());
    lithia::flush().await;
    rows.set(vec![row(1, "A")]);
    lithia::flush().await;
    assert_eq!(*first.get(), "A");
    assert_eq!(seen.load(Ordering::SeqCst), 4);
}

// Case BF: a keyed node is reclaimed only with the same parents; otherwise a new one is created.
#[lithia::test]
async fn case_bf_new_parents_make_a_new_node() {
    let a: SourceNode<u32> = effect!(1);
    let b: SourceNode<u32> = effect!(10);
    let use_a: SourceNode<bool> = effect!(true);
    let base: usize = lithia::test::live_nodes().await;
    let made: DerivedNode<Option<DerivedNode<u32>>> = effect!(|use_a| {
        let dep: SourceNode<u32> = if *use_a.get() { a.clone() } else { b.clone() };
        Some(effect!(|dep| *dep.get(), key = 0u8))
    });
    let current = || -> DerivedNode<u32> { (*made.get()).clone().expect("made") };
    lithia::flush().await;
    let first: DerivedNode<u32> = current();
    assert_eq!(*first.get(), 1);
    use_a.set(false);
    lithia::flush().await;
    assert_eq!(*current().get(), 10);
    // The old node was not reclaimed: it keeps following `a` only while `first` holds it.
    a.set(2);
    lithia::flush().await;
    assert_eq!((*first.get(), *current().get()), (2, 10));
    drop(first);
    assert_eq!(lithia::test::live_nodes().await, base + 2);
}

// Case BG: duplicate keys are matched in creation order and warned about.
#[lithia::test]
async fn case_bg_duplicate_keys_match_in_order() {
    let _events: Events = events();
    let rows: SourceNode<Vec<Row>> = effect!(vec![row(1, "a"), row(1, "b")]);
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let list: DerivedNode<Vec<DerivedNode<String>>> = effect!(|rows| {
        rows.get()
            .iter()
            .map(|row: &Row| {
                effect!(
                    |rows| {
                        let _ = rows.get();
                        runs.fetch_add(1, Ordering::SeqCst);
                        row.title.clone()
                    },
                    key = row.id
                )
            })
            .collect::<Vec<DerivedNode<String>>>()
    });
    lithia::flush().await;
    // Each run of `list` reruns the two rows too, since they depend on `rows`; equal captures add nothing.
    assert_eq!((texts(&list), runs.load(Ordering::SeqCst)), (vec!["a".to_string(), "b".to_string()], 2));
    assert!(reported(Level::WARN, "same key"));
    rows.set(vec![row(1, "b"), row(1, "a")]);
    lithia::flush().await;
    assert_eq!(texts(&list), ["b", "a"]);
}

// Case BH: keyed async nodes keep running when their captures are equal and restart when not.
#[lithia::test]
async fn case_bh_keyed_async_nodes() {
    let rows: SourceNode<Vec<Row>> = effect!(vec![row(1, "a")]);
    let tick: SourceNode<u32> = effect!(0);
    let starts: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let blocks: DerivedNode<Vec<SourceNode<String>>> = effect!(|rows| {
        rows.get()
            .iter()
            .map(|row: &Row| {
                effect!(
                    async {
                        starts.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        row.title.to_uppercase()
                    },
                    key = row.id
                )
            })
            .collect::<Vec<SourceNode<String>>>()
    });
    let derived: DerivedNode<Vec<DerivedNode<String>>> = effect!(|rows| {
        rows.get()
            .iter()
            .map(|row: &Row| {
                effect!(
                    async |tick| {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        format!("{}{}", row.title, tick.get())
                    },
                    key = row.id
                )
            })
            .collect::<Vec<DerivedNode<String>>>()
    });
    lithia::flush().await;
    let block: SourceNode<String> = blocks.get()[0].clone();
    let item: DerivedNode<String> = derived.get()[0].clone();
    lithia::test::settled(&block).await;
    lithia::test::settled(&item).await;
    assert_eq!((block.get().as_str(), item.get().as_str(), starts.load(Ordering::SeqCst)), ("A", "a0", 1));

    // An equal row: the block is not restarted and the derived node does not rerun.
    let value: Arc<String> = item.get();
    rows.set(vec![row(1, "a")]);
    lithia::flush().await;
    lithia::test::settled(&blocks).await;
    lithia::test::settled(&derived).await;
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert!(Arc::ptr_eq(&value, &derived.get()[0].get()));

    // A changed row restarts the block and reruns the derived node, both in place.
    rows.set(vec![row(1, "b")]);
    lithia::flush().await;
    lithia::test::settled(&block).await;
    lithia::test::settled(&item).await;
    assert_eq!((block.get().as_str(), item.get().as_str(), starts.load(Ordering::SeqCst)), ("B", "b0", 2));
    tick.set(1);
    lithia::flush().await;
    lithia::test::settled(&item).await;
    assert_eq!(*item.get(), "b1");
}

// Case BI: a superseded creator's keyed nodes wait for its follow-up run, which reclaims them.
#[lithia::test]
async fn case_bi_a_superseded_run_hands_its_keyed_nodes_on() {
    let rows: SourceNode<Vec<Row>> = effect!(vec![row(1, "a"), row(2, "b")]);
    let seed: SourceNode<u32> = effect!(0);
    let gate: Arc<tokio::sync::Notify> = Arc::new(tokio::sync::Notify::new());
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let list: DerivedNode<Vec<DerivedNode<String>>> = effect!(async |rows| {
        let current: Arc<Vec<Row>> = rows.get();
        let seed: SourceNode<u32> = seed.clone();
        let made: Vec<DerivedNode<String>> = current
            .iter()
            .map(|row: &Row| {
                effect!(
                    |seed| {
                        let _ = seed.get();
                        runs.fetch_add(1, Ordering::SeqCst);
                        row.title.clone()
                    },
                    key = row.id
                )
            })
            .collect();
        if current.len() == 3 {
            gate.notified().await;
        }
        made
    });
    lithia::flush().await;
    lithia::test::settled(&list).await;
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    // The run for three rows parks after creating row 3; a fourth row supersedes it.
    rows.set(vec![row(1, "a"), row(2, "b"), row(3, "c")]);
    lithia::flush().await;
    while runs.load(Ordering::SeqCst) < 3 {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    rows.set(vec![row(1, "a"), row(2, "b"), row(3, "c"), row(4, "d")]);
    lithia::flush().await;
    lithia::test::settled(&list).await;
    assert_eq!(texts(&list), ["a", "b", "c", "d"]);
    // Rows 1–3 were reclaimed through the superseded run with equal captures; only row 4 is new.
    assert_eq!(runs.load(Ordering::SeqCst), 4);
}

// Case BJ: a key outside any body has nothing to reclaim; the node is created as usual.
#[lithia::test]
async fn case_bj_a_key_outside_a_body() {
    let _events: Events = events();
    let base: usize = lithia::test::live_nodes().await;
    let made: Vec<SourceNode<u32>> = (0..2).map(|_| effect!(7u32, key = 1u8)).collect();
    made[0].set(8);
    assert_eq!((*made[0].get(), *made[1].get()), (8, 7));
    assert_eq!(lithia::test::live_nodes().await, base + 2);
    assert!(reported(Level::DEBUG, "outside a body"));
}

// Case BK: an `Arc` capture compares by pointer; a capture without `PartialEq` always counts as changed.
#[lithia::test]
async fn case_bk_captures_without_partial_eq() {
    struct Service(AtomicU32);
    #[derive(Clone)]
    struct Opaque(u32);
    let service: Arc<Service> = Arc::new(Service(AtomicU32::new(0)));
    let opaque: Opaque = Opaque(0);
    let rows: SourceNode<Vec<Row>> = effect!(vec![row(1, "a")]);
    let tick: SourceNode<u32> = effect!(0);
    let by_arc: DerivedNode<Vec<DerivedNode<u32>>> = effect!(|rows| {
        rows.get()
            .iter()
            .map(|row: &Row| {
                effect!(
                    |tick| {
                        let _ = tick.get();
                        service.0.fetch_add(1, Ordering::SeqCst) + row.id
                    },
                    key = row.id
                )
            })
            .collect::<Vec<DerivedNode<u32>>>()
    });
    let opaque_runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let by_value: DerivedNode<Vec<DerivedNode<u32>>> = effect!(|rows| {
        rows.get()
            .iter()
            .map(|row: &Row| {
                effect!(
                    |tick| {
                        let _ = tick.get();
                        opaque_runs.fetch_add(1, Ordering::SeqCst) + opaque.0 + row.id
                    },
                    key = row.id
                )
            })
            .collect::<Vec<DerivedNode<u32>>>()
    });
    lithia::flush().await;
    rows.set(vec![row(1, "a")]);
    lithia::flush().await;
    let _ = (by_arc.get(), by_value.get());
    // The same `Arc`: no rerun. `Opaque` has no `PartialEq`: a rerun on every reclaim.
    assert_eq!(service.0.load(Ordering::SeqCst), 1);
    assert_eq!(opaque_runs.load(Ordering::SeqCst), 2);
}
