// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Keyed nodes reclaimed across a creator's runs (spec §10.1).

use super::*;

/// The keyed form: the child waits until its creator's run has reclaimed it with new captures.
#[test]
fn a_creator_reclaims_its_keyed_child_before_it_runs() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let seen: std::sync::Arc<std::sync::Mutex<Vec<(u32, u32)>>> = Default::default();
            let input: SourceNode<u32> = scope.batch(|| {
                let input: SourceNode<u32> = effect!(0);
                effect!(|input| {
                    let label: u32 = *input.get();
                    effect!(
                        |input| {
                            let pair: (u32, u32) = (*label, *input.get());
                            seen.lock().unwrap().push(pair);
                        },
                        key = 0u8
                    );
                });
                input
            });
            scope.flush();
            scope.batch(|| input.set(1));
            scope.flush();
            let runs: Vec<(u32, u32)> = seen.lock().unwrap().clone();
            assert_eq!(runs, [(0, 0), (1, 1)]);
        });
    });
}

/// A keyed child is reclaimed by its creator's rerun on the worker while its last outside handle
/// drops on the main thread: the holdover keeps it, and with equal captures it does not run again.
#[test]
fn a_keyed_child_is_reclaimed_while_its_handle_drops() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (tick, k, runs, list): (
                SourceNode<u32>,
                SourceNode<u32>,
                SourceNode<u32>,
                DerivedNode<Option<DerivedNode<u32>>>,
            ) = scope.batch(|| {
                let tick: SourceNode<u32> = effect!(0);
                let k: SourceNode<u32> = effect!(1);
                let runs: SourceNode<u32> = effect!(0);
                let list: DerivedNode<Option<DerivedNode<u32>>> = effect!(|tick| {
                    let _ = tick.get();
                    Some(effect!(
                        |k| {
                            runs.update(|n: &u32| n + 1);
                            *k.get()
                        },
                        key = 0u8
                    ))
                });
                (tick, k, runs, list)
            });
            scope.flush();
            let child: DerivedNode<u32> = (*list.get()).clone().expect("child");
            scope.batch(|| tick.set(1));
            drop(child);
            scope.flush();
            assert_eq!(*runs.get(), 1);
            assert_eq!(scope.live(), 5);
            drop(k);
        });
    });
}

/// A creator is disposed while its rerun reclaims a keyed child: the child is let go with the
/// creator's holdover, whichever comes first.
#[test]
fn a_creator_is_disposed_while_its_run_reclaims() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (tick, k, creator): (SourceNode<u32>, SourceNode<u32>, DerivedNode<()>) = scope.batch(|| {
                let tick: SourceNode<u32> = effect!(0);
                let k: SourceNode<u32> = effect!(1);
                let creator: DerivedNode<()> = effect!(|tick| {
                    let _ = tick.get();
                    effect!(
                        |k| {
                            let _ = k.get();
                        },
                        key = 0u8
                    );
                });
                (tick, k, creator)
            });
            scope.flush();
            scope.batch(|| tick.set(1));
            lithia::dispose(&creator);
            scope.flush();
            // A disposed node's run no longer counts for `flush`, so a body still running on the
            // worker may hold a creation it has not dropped yet; a leak would spin here.
            while scope.live() > 2 {
                if !model::run_one() {
                    loom::thread::yield_now();
                }
            }
            drop(k);
        });
    });
}

/// A write supersedes a creator's run after it created a keyed child: the stopped run's keyed
/// nodes wait in the holdover, and the follow-up run reclaims the child instead of a new one.
#[test]
fn a_superseded_creator_hands_its_keyed_child_on() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let created: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
            let (id, k, runs, _loader): (SourceNode<u32>, SourceNode<u32>, SourceNode<u32>, DerivedNode<()>) = scope
                .batch(|| {
                    let id: SourceNode<u32> = effect!(7);
                    let k: SourceNode<u32> = effect!(1);
                    let runs: SourceNode<u32> = effect!(0);
                    let loader: DerivedNode<()> = effect!(|id| {
                        let _ = id.get();
                        effect!(
                            |k| {
                                let _ = k.get();
                                runs.update(|n: &u32| n + 1);
                            },
                            key = 0u8
                        );
                        created.store(true, Ordering::SeqCst);
                        loom::thread::yield_now(); // the write may land here
                    });
                    (id, k, runs, loader)
                });
            wait_for(&created);
            scope.batch(|| id.set(8));
            scope.flush();
            assert_eq!(*runs.get(), 1);
            assert_eq!(scope.live(), 5);
            drop(k);
        });
    });
}
