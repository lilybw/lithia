// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Keyed nodes: holdover, reclaiming and refreshing across a creator's runs (spec §10.1, Keyed nodes).

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use lithia::{Accessor, DerivedNode, Ref, SourceNode, node};

fn count(counter: &AtomicU32) -> u32 {
    counter.load(Ordering::SeqCst)
}

#[lithia::test]
async fn the_holdover_keeps_a_handleless_child_until_its_creator_stops_making_it() {
    let make: SourceNode<bool> = node!(true);
    let tick: SourceNode<u32> = node!(0);
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let base: usize = lithia::test::live_nodes().await;
    let _creator: DerivedNode<()> = node!(|make| {
        if *make.get() {
            node!(
                |tick| {
                    let _ = tick.get();
                    runs.fetch_add(1, Ordering::SeqCst);
                },
                key = 0u8
            );
        }
    });
    lithia::flush().await;
    assert_eq!((count(&runs), lithia::test::live_nodes().await), (1, base + 2));
    make.set(true);
    lithia::flush().await;
    // Reclaimed with equal captures: no rerun, no new node.
    assert_eq!((count(&runs), lithia::test::live_nodes().await), (1, base + 2));
    tick.set(1);
    lithia::flush().await;
    assert_eq!(count(&runs), 2);
    make.set(false);
    lithia::flush().await;
    assert_eq!(lithia::test::live_nodes().await, base + 1);
    tick.set(2);
    lithia::flush().await;
    assert_eq!(count(&runs), 2);
}

#[lithia::test]
async fn disposing_the_creator_frees_its_keyed_children() {
    let rows: SourceNode<Vec<u32>> = node!(vec![1, 2, 3]);
    let base: usize = lithia::test::live_nodes().await;
    let list: DerivedNode<Vec<SourceNode<u32>>> =
        node!(|rows| rows.get().iter().map(|id: &u32| node!(*id * 10, key = *id)).collect::<Vec<SourceNode<u32>>>());
    lithia::flush().await;
    rows.set(vec![3, 1]);
    lithia::flush().await;
    assert_eq!(lithia::test::live_nodes().await, base + 3);
    lithia::dispose(&list);
    drop(list);
    assert_eq!(lithia::test::live_nodes().await, base);
}

#[lithia::test]
async fn disposing_a_creator_mid_run_frees_its_holdover() {
    let make: SourceNode<bool> = node!(true);
    let gate: Arc<tokio::sync::Notify> = Arc::new(tokio::sync::Notify::new());
    let parked: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let base: usize = lithia::test::live_nodes().await;
    let creator: DerivedNode<()> = node!(async |make| {
        if *make.get() {
            node!(1u32, key = 0u8);
        } else {
            parked.fetch_add(1, Ordering::SeqCst);
            gate.notified().await;
        }
    });
    lithia::flush().await;
    lithia::test::settled(&creator).await;
    assert_eq!(lithia::test::live_nodes().await, base + 2);
    make.set(false);
    lithia::flush().await;
    while count(&parked) == 0 {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    // The parked run did not reclaim the child, which waits in the holdover.
    assert_eq!(lithia::test::live_nodes().await, base + 2);
    lithia::dispose(&creator);
    assert_eq!(lithia::test::live_nodes().await, base);
}

#[lithia::test]
async fn a_scoped_keyed_child_is_never_reclaimed() {
    let tick: SourceNode<u32> = node!(0);
    let rerun: SourceNode<u32> = node!(0);
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let holder: DerivedNode<Option<DerivedNode<u32>>> = node!(|rerun| {
        let _ = rerun.get();
        Some(
            node!(
                |tick| {
                    runs.fetch_add(1, Ordering::SeqCst);
                    *tick.get()
                },
                key = 0u8
            )
            .scoped(),
        )
    });
    lithia::flush().await;
    rerun.set(1);
    lithia::flush().await;
    let _ = holder.get();
    // Scoped, it left the generation, so the second run created a node of its own.
    assert_eq!(count(&runs), 2);
}

#[lithia::test]
async fn a_new_write_target_makes_a_new_node() {
    let x: SourceNode<u32> = node!(0);
    let y: SourceNode<u32> = node!(0);
    let to_x: SourceNode<bool> = node!(true);
    let tick: SourceNode<u32> = node!(0);
    let firsts: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let writer: DerivedNode<Option<DerivedNode<()>>> = node!(|to_x| {
        let target: SourceNode<u32> = if *to_x.get() { x.clone() } else { y.clone() };
        Some(node!(
            |tick| {
                if *tick.get() == 0 {
                    firsts.fetch_add(1, Ordering::SeqCst);
                }
                target.set(*tick.get());
            },
            key = 0u8
        ))
    });
    lithia::flush().await;
    let first: DerivedNode<()> = (*writer.get()).clone().expect("writer");
    to_x.set(false);
    lithia::flush().await;
    // Its body would write `y` now, a node it has no edge to: a new node took the key, and the
    // first one, held here, still writes `x`.
    assert_eq!(count(&firsts), 2);
    tick.set(5);
    lithia::flush().await;
    assert_eq!((*x.get(), *y.get()), (5, 5));
    drop(first);
}

#[lithia::test]
async fn a_swapped_read_handle_reruns_the_same_node() {
    let a: SourceNode<u32> = node!(1);
    let b: SourceNode<u32> = node!(2);
    let use_a: SourceNode<bool> = node!(true);
    let tick: SourceNode<u32> = node!(0);
    let shown: DerivedNode<Option<DerivedNode<u32>>> = node!(|use_a| {
        let source: SourceNode<u32> = if *use_a.get() { a.clone() } else { b.clone() };
        Some(node!(|tick| *source.get() + *tick.get(), key = 0u8))
    });
    lithia::flush().await;
    let first: DerivedNode<u32> = (*shown.get()).clone().expect("shown");
    assert_eq!(*first.get(), 1);
    use_a.set(false);
    lithia::flush().await;
    // The capture changed, the node did not: the handle taken before now reads `b`.
    assert_eq!(*first.get(), 2);
}

#[derive(Clone, PartialEq)]
struct Label {
    name: String,
}

impl Label {
    fn node(&self, tick: &SourceNode<u32>, runs: &Arc<AtomicU32>) -> DerivedNode<String> {
        let tick: SourceNode<u32> = tick.clone();
        let runs: Arc<AtomicU32> = runs.clone();
        node!(
            |tick| {
                runs.fetch_add(1, Ordering::SeqCst);
                format!("{}{}", self.name, tick.get())
            },
            key = 0u8
        )
    }
}

#[lithia::test]
async fn self_is_compared_by_value() {
    let label: Label = Label { name: "n".to_string() };
    let tick: SourceNode<u32> = node!(0);
    let rerun: SourceNode<u32> = node!(0);
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let made: DerivedNode<Option<DerivedNode<String>>> = node!(|rerun| {
        let _ = rerun.get();
        Some(label.node(tick, runs))
    });
    lithia::flush().await;
    rerun.set(1);
    lithia::flush().await;
    assert_eq!(count(&runs), 1);
    assert_eq!(*(*made.get()).clone().expect("made").get(), "n0");
}

fn keyed_source<T: Clone + PartialEq + Send + Sync + 'static>(value: T) -> SourceNode<T> {
    node!(value, key = 0u8)
}

#[lithia::test]
async fn one_site_with_two_value_types_never_mixes_them() {
    let rerun: SourceNode<u32> = node!(0);
    let pair: DerivedNode<Option<(SourceNode<u32>, SourceNode<String>)>> = node!(|rerun| {
        // The second run creates them the other way round, so only the value type tells them apart.
        if *rerun.get() == 0 {
            let number: SourceNode<u32> = keyed_source(1u32);
            Some((number, keyed_source("x".to_string())))
        } else {
            let text: SourceNode<String> = keyed_source("x".to_string());
            Some((keyed_source(1u32), text))
        }
    });
    lithia::flush().await;
    let (number, text): (SourceNode<u32>, SourceNode<String>) = (*pair.get()).clone().expect("pair");
    let (number_value, text_value): (Ref<u32>, Ref<String>) = (number.get(), text.get());
    rerun.set(1);
    lithia::flush().await;
    assert!(matches!(lithia::status(&pair), lithia::Status::Ready));
    let (again_number, again_text): (SourceNode<u32>, SourceNode<String>) = (*pair.get()).clone().expect("pair");
    assert!(Ref::ptr_eq(&number_value, &again_number.get()));
    assert!(Ref::ptr_eq(&text_value, &again_text.get()));
}

#[lithia::test]
async fn a_keyed_generator_is_reclaimed() {
    let rerun: SourceNode<u32> = node!(0);
    let tick: SourceNode<u32> = node!(0);
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let made: DerivedNode<Option<DerivedNode<u32>>> = node!(|rerun| {
        let _ = rerun.get();
        Some(node!(
            |tick| {
                runs.fetch_add(1, Ordering::SeqCst);
                yield *tick.get();
                yield *tick.get() + 1;
            },
            key = 0u8
        ))
    });
    lithia::flush().await;
    let generator: DerivedNode<u32> = (*made.get()).clone().expect("made");
    lithia::test::settled(&generator).await;
    rerun.set(1);
    lithia::flush().await;
    lithia::test::settled(&made).await;
    assert_eq!((count(&runs), *generator.get()), (1, 1));
    tick.set(5);
    lithia::flush().await;
    lithia::test::settled(&generator).await;
    assert_eq!((count(&runs), *generator.get()), (2, 6));
}

#[lithia::test]
async fn keys_of_other_sites_never_meet() {
    let rerun: SourceNode<u32> = node!(0);
    let made: DerivedNode<Option<(SourceNode<u32>, SourceNode<u32>)>> = node!(|rerun| {
        let _ = rerun.get();
        Some((node!(1u32, key = 0u8), node!(2u32, key = 0u8)))
    });
    lithia::flush().await;
    rerun.set(1);
    lithia::flush().await;
    let (one, two): (SourceNode<u32>, SourceNode<u32>) = (*made.get()).clone().expect("made");
    assert_eq!((*one.get(), *two.get()), (1, 2));
}

#[lithia::test]
async fn a_different_shape_makes_a_new_node() {
    let strict: SourceNode<bool> = node!(true);
    let rerun: SourceNode<u32> = node!(0);
    let tick: SourceNode<u32> = node!(0);
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let _creator: DerivedNode<()> = node!(|strict, rerun| {
        let _ = rerun.get();
        node!(
            |tick| {
                let _ = tick.get();
                runs.fetch_add(1, Ordering::SeqCst);
            },
            key = 0u8,
            resilience = if *strict.get() { NONE } else { PARTIAL }
        );
    });
    lithia::flush().await;
    assert_eq!(count(&runs), 1);
    rerun.set(1);
    lithia::flush().await;
    // Same captures, same shape: reclaimed without a run.
    assert_eq!(count(&runs), 1);
    strict.set(false);
    lithia::flush().await;
    // Same captures, other resilience: a new node, which runs.
    assert_eq!(count(&runs), 2);
}

#[lithia::test]
async fn a_reclaimed_node_takes_the_new_comparator() {
    let input: SourceNode<u32> = node!(0);
    let coarse: SourceNode<bool> = node!(false);
    let made: DerivedNode<Option<DerivedNode<u32>>> = node!(|coarse| {
        let by_tens: bool = *coarse.get();
        Some(node!(|input| *input.get(), key = 0u8, equals = move |a: &u32, b: &u32| by_tens && a / 10 == b / 10))
    });
    lithia::flush().await;
    let child: DerivedNode<u32> = (*made.get()).clone().expect("made");
    coarse.set(true);
    lithia::flush().await;
    // Reclaimed unchanged, with the comparator that compares by tens: 5 equals 0.
    assert!(Ref::ptr_eq(&child.get(), &(*made.get()).clone().expect("made").get()));
    input.set(5);
    lithia::flush().await;
    assert_eq!(*child.get(), 0);
    input.set(12);
    lithia::flush().await;
    assert_eq!(*child.get(), 12);
}

#[lithia::test]
async fn a_refreshed_source_is_written_with_the_new_comparator() {
    let config: SourceNode<(bool, u32)> = node!((false, 5));
    let made: DerivedNode<Option<SourceNode<u32>>> = node!(|config| {
        let (by_tens, value): (bool, u32) = *config.get();
        Some(node!(value, key = 0u8, equals = move |a: &u32, b: &u32| by_tens && a / 10 == b / 10))
    });
    lithia::flush().await;
    // The value changes along with the comparator: 0 is written with the new one, which finds it
    // equal to 5, so the node keeps 5.
    config.set((true, 0));
    lithia::flush().await;
    assert_eq!(*(*made.get()).clone().expect("made").get(), 5);
}

/// The source a [`WritingKey`] writes from another thread the first time it is compared.
static WRITTEN_BY_KEY: std::sync::OnceLock<SourceNode<u32>> = std::sync::OnceLock::new();
static KEY_WROTE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A key whose comparison waits for a write to its creator's parent on another thread.
struct WritingKey(u32);

impl std::hash::Hash for WritingKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl PartialEq for WritingKey {
    fn eq(&self, other: &WritingKey) -> bool {
        if !KEY_WROTE.swap(true, Ordering::SeqCst)
            && let Some(input) = WRITTEN_BY_KEY.get()
        {
            let input: SourceNode<u32> = input.clone();
            std::thread::spawn(move || input.update(|n: &u32| n + 1)).join().unwrap();
        }
        self.0 == other.0
    }
}

impl Eq for WritingKey {}

#[lithia::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_comparison_runs_outside_the_creators_lock() {
    let input: SourceNode<u32> = node!(0);
    let _ = WRITTEN_BY_KEY.set(input.clone());
    let tick: SourceNode<u32> = node!(0);
    let child_runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let creator: DerivedNode<u32> = node!(|input| {
        let _child: DerivedNode<()> = node!(
            |tick| {
                let _ = tick.get();
                child_runs.fetch_add(1, Ordering::SeqCst);
            },
            key = WritingKey(7)
        );
        *input.get()
    });
    lithia::flush().await;
    input.set(10); // the rerun reclaims the child, comparing its key, which writes `input` again
    let flushed: Result<(), tokio::time::error::Elapsed> =
        tokio::time::timeout(std::time::Duration::from_secs(5), lithia::flush()).await;
    assert!(flushed.is_ok(), "a write waited on by a key comparison deadlocked against the creator's lock");
    lithia::test::settled(&creator).await;
    assert_eq!(*creator.get(), 11);
    assert_eq!(count(&child_runs), 1, "the child was reclaimed each time");
}

/// A key whose values all hash alike, so only equality tells two of them apart.
#[derive(PartialEq, Eq)]
struct Colliding(u32);

impl std::hash::Hash for Colliding {
    fn hash<H: std::hash::Hasher>(&self, _state: &mut H) {}
}

#[lithia::test]
async fn keys_that_hash_alike_name_lineages_of_their_own() {
    let _ = lithia_test::events();
    let go: SourceNode<u32> = node!(0);
    let shared: SourceNode<u32> = node!(0);
    let out: SourceNode<u32> = node!(0);
    let _creator: DerivedNode<()> = node!(|go| {
        for first in [true, false] {
            // The second runs on the first's write; with one lineage for both, it would be cut.
            let input: Accessor<u32> = if first { go.accessor() } else { shared.accessor() };
            node!(
                |input| {
                    if *first {
                        shared.set(*input.get() + 1);
                    } else {
                        out.set(*input.get() * 10);
                    }
                },
                key = Colliding(u32::from(first))
            );
        }
    });
    lithia::flush().await;
    go.set(5);
    lithia::test::settled(&go).await;
    assert_eq!((*shared.get(), *out.get()), (6, 60));
    let here: &str = "keys_that_hash_alike_name_lineages_of_their_own";
    let cut: Option<lithia_test::Captured> =
        lithia_test::find(tracing::Level::DEBUG, &["a cycle cut an effect", here], Duration::from_millis(200)).await;
    assert!(cut.is_none(), "{cut:?}");
}

/// A key that counts the values made and dropped; it compares and hashes by its number alone.
struct CountedKey(u32, Arc<(AtomicU32, AtomicU32)>);

impl CountedKey {
    fn new(number: u32, counts: &Arc<(AtomicU32, AtomicU32)>) -> CountedKey {
        counts.0.fetch_add(1, Ordering::SeqCst);
        CountedKey(number, counts.clone())
    }
}

impl PartialEq for CountedKey {
    fn eq(&self, other: &CountedKey) -> bool {
        self.0 == other.0
    }
}

impl Eq for CountedKey {}

impl std::hash::Hash for CountedKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl Drop for CountedKey {
    fn drop(&mut self) {
        self.1.1.fetch_add(1, Ordering::SeqCst);
    }
}

#[lithia::test]
async fn keys_a_creator_stops_making_are_let_go_of() {
    let keys: SourceNode<u32> = node!(0);
    let counts: Arc<(AtomicU32, AtomicU32)> = Arc::new((AtomicU32::new(0), AtomicU32::new(0)));
    let made: Arc<(AtomicU32, AtomicU32)> = counts.clone();
    let _creator: DerivedNode<()> = node!(|keys| {
        let number: u32 = *keys.get();
        node!(
            |keys| {
                let _ = keys.get();
            },
            key = CountedKey::new(number, made)
        );
    });
    lithia::flush().await;
    for number in 1..=12 {
        keys.set(number); // a key the creator never made before; the one before it is made no more
        lithia::flush().await;
    }
    let _ = lithia::test::live_nodes().await;
    let live: u32 = counts.0.load(Ordering::SeqCst) - counts.1.load(Ordering::SeqCst);
    assert!(live <= 4, "{live} of {} keys still held", counts.0.load(Ordering::SeqCst));
}

/// A service with no `PartialEq`: captured in an `Arc`, it compares by pointer.
struct Service(u32);

#[lithia::test]
async fn a_keyed_node_reruns_when_a_captured_arc_is_another_one() {
    let config: SourceNode<Arc<Service>> = node!(Arc::new(Service(1)));
    let tick: SourceNode<u32> = node!(0);
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let _creator: DerivedNode<Vec<DerivedNode<u32>>> = node!(|config| {
        let shared: Arc<Service> = (*config.get()).clone();
        vec![node!(
            |tick| {
                let _ = tick.get();
                runs.fetch_add(1, Ordering::SeqCst);
                shared.0
            },
            key = 0u8
        )]
    });
    lithia::flush().await;
    assert_eq!(count(&runs), 1);
    let same: Arc<Service> = (*config.get()).clone();
    config.set(same); // the same `Arc`: the reclaimed node does not rerun
    lithia::flush().await;
    assert_eq!(count(&runs), 1);
    config.set(Arc::new(Service(1))); // another `Arc`, though alike
    lithia::flush().await;
    assert_eq!(count(&runs), 2);
}

/// A key that notes the thread it drops on; compares and hashes by its number alone.
struct KeyDropsWhere(u32, Arc<std::sync::Mutex<Vec<std::thread::ThreadId>>>);

impl PartialEq for KeyDropsWhere {
    fn eq(&self, other: &KeyDropsWhere) -> bool {
        self.0 == other.0
    }
}

impl Eq for KeyDropsWhere {}

impl std::hash::Hash for KeyDropsWhere {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl Drop for KeyDropsWhere {
    fn drop(&mut self) {
        self.1.lock().unwrap().push(std::thread::current().id());
    }
}

#[lithia::test(flavor = "current_thread")]
async fn keys_drop_off_the_thread_that_runs_their_creator() {
    let threads: Arc<std::sync::Mutex<Vec<std::thread::ThreadId>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let keys: SourceNode<u32> = node!(0);
    let noted: Arc<std::sync::Mutex<Vec<std::thread::ThreadId>>> = threads.clone();
    let _creator: DerivedNode<()> = node!(|keys| {
        let number: u32 = *keys.get();
        node!(
            |keys| {
                let _ = keys.get();
            },
            key = KeyDropsWhere(number, noted.clone())
        );
    });
    lithia::flush().await;
    for number in 1..=6 {
        keys.set(number); // the creator runs here, on the only worker, and lets go of the key before
        lithia::flush().await;
    }
    let _ = lithia::test::live_nodes().await;
    let here: std::thread::ThreadId = std::thread::current().id();
    let dropped: Vec<std::thread::ThreadId> = threads.lock().unwrap().clone();
    assert!(dropped.len() >= 5, "{dropped:?}");
    assert!(dropped.iter().all(|thread: &std::thread::ThreadId| *thread != here), "{dropped:?}");
}
