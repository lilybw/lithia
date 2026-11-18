// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Keyed nodes: holdover, reclaiming and refreshing across a creator's runs (spec §10.1, Keyed nodes).

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lithia::{DerivedNode, SourceNode, effect};

fn count(counter: &AtomicU32) -> u32 {
    counter.load(Ordering::SeqCst)
}

#[lithia::test]
async fn the_holdover_keeps_a_handleless_child_until_its_creator_stops_making_it() {
    let make: SourceNode<bool> = effect!(true);
    let tick: SourceNode<u32> = effect!(0);
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let base: usize = lithia::test::live_nodes().await;
    let _creator: DerivedNode<()> = effect!(|make| {
        if *make.get() {
            effect!(
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
    let rows: SourceNode<Vec<u32>> = effect!(vec![1, 2, 3]);
    let base: usize = lithia::test::live_nodes().await;
    let list: DerivedNode<Vec<SourceNode<u32>>> = effect!(|rows| rows
        .get()
        .iter()
        .map(|id: &u32| effect!(*id * 10, key = *id))
        .collect::<Vec<SourceNode<u32>>>());
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
    let make: SourceNode<bool> = effect!(true);
    let gate: Arc<tokio::sync::Notify> = Arc::new(tokio::sync::Notify::new());
    let parked: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let base: usize = lithia::test::live_nodes().await;
    let creator: DerivedNode<()> = effect!(async |make| {
        if *make.get() {
            effect!(1u32, key = 0u8);
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
    let tick: SourceNode<u32> = effect!(0);
    let rerun: SourceNode<u32> = effect!(0);
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let holder: DerivedNode<Option<DerivedNode<u32>>> = effect!(|rerun| {
        let _ = rerun.get();
        Some(
            effect!(
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
    let x: SourceNode<u32> = effect!(0);
    let y: SourceNode<u32> = effect!(0);
    let to_x: SourceNode<bool> = effect!(true);
    let tick: SourceNode<u32> = effect!(0);
    let firsts: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let writer: DerivedNode<Option<DerivedNode<()>>> = effect!(|to_x| {
        let target: SourceNode<u32> = if *to_x.get() { x.clone() } else { y.clone() };
        Some(effect!(
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
    let a: SourceNode<u32> = effect!(1);
    let b: SourceNode<u32> = effect!(2);
    let use_a: SourceNode<bool> = effect!(true);
    let tick: SourceNode<u32> = effect!(0);
    let shown: DerivedNode<Option<DerivedNode<u32>>> = effect!(|use_a| {
        let source: SourceNode<u32> = if *use_a.get() { a.clone() } else { b.clone() };
        Some(effect!(|tick| *source.get() + *tick.get(), key = 0u8))
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
        effect!(
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
    let tick: SourceNode<u32> = effect!(0);
    let rerun: SourceNode<u32> = effect!(0);
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let made: DerivedNode<Option<DerivedNode<String>>> = effect!(|rerun| {
        let _ = rerun.get();
        Some(label.node(&tick, &runs))
    });
    lithia::flush().await;
    rerun.set(1);
    lithia::flush().await;
    assert_eq!(count(&runs), 1);
    assert_eq!(*(*made.get()).clone().expect("made").get(), "n0");
}

fn keyed_source<T: Clone + PartialEq + Send + Sync + 'static>(value: T) -> SourceNode<T> {
    effect!(value, key = 0u8)
}

#[lithia::test]
async fn one_site_with_two_value_types_never_mixes_them() {
    let rerun: SourceNode<u32> = effect!(0);
    let pair: DerivedNode<Option<(SourceNode<u32>, SourceNode<String>)>> = effect!(|rerun| {
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
    let (number_value, text_value): (Arc<u32>, Arc<String>) = (number.get(), text.get());
    rerun.set(1);
    lithia::flush().await;
    assert!(matches!(lithia::status(&pair), lithia::Status::Ready));
    let (again_number, again_text): (SourceNode<u32>, SourceNode<String>) = (*pair.get()).clone().expect("pair");
    assert!(Arc::ptr_eq(&number_value, &again_number.get()));
    assert!(Arc::ptr_eq(&text_value, &again_text.get()));
}

#[lithia::test]
async fn a_keyed_generator_is_reclaimed() {
    let rerun: SourceNode<u32> = effect!(0);
    let tick: SourceNode<u32> = effect!(0);
    let runs: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let made: DerivedNode<Option<DerivedNode<u32>>> = effect!(|rerun| {
        let _ = rerun.get();
        Some(effect!(
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
    let rerun: SourceNode<u32> = effect!(0);
    let made: DerivedNode<Option<(SourceNode<u32>, SourceNode<u32>)>> = effect!(|rerun| {
        let _ = rerun.get();
        Some((effect!(1u32, key = 0u8), effect!(2u32, key = 0u8)))
    });
    lithia::flush().await;
    rerun.set(1);
    lithia::flush().await;
    let (one, two): (SourceNode<u32>, SourceNode<u32>) = (*made.get()).clone().expect("made");
    assert_eq!((*one.get(), *two.get()), (1, 2));
}
