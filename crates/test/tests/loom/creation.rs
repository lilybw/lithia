// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Nodes created by runs, and their generations (spec §5, §10.2).

use super::*;

/// A write reaches a creator and the child it made, run on the main thread and the worker: the old
/// child waits for its creator, which replaces it, so it never sees the new value.
#[test]
fn a_creator_decides_its_child_before_it_runs() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let seen: std::sync::Arc<std::sync::Mutex<Vec<(u32, u32)>>> = Default::default();
            let input: SourceNode<u32> = scope.batch(|| {
                let input: SourceNode<u32> = effect!(0);
                effect!(|input| {
                    let made_at: u32 = *input.get();
                    effect!(|input| {
                        // Read before locking: a std lock held across a loom switch would block the model.
                        let pair: (u32, u32) = (*made_at, *input.get());
                        seen.lock().unwrap().push(pair);
                    });
                });
                input
            });
            scope.flush();
            scope.batch(|| input.set(1));
            scope.flush();
            let runs: Vec<(u32, u32)> = seen.lock().unwrap().clone();
            assert!(runs.iter().all(|(made_at, value): &(u32, u32)| made_at == value), "{runs:?}");
            assert_eq!(runs.last(), Some(&(1, 1)));
        });
    });
}

/// A write supersedes an async creator's run that created a child: the child waits for the
/// restart, which lets go of it, and never runs on the input that superseded its creator.
#[test]
fn a_superseded_runs_creation_waits_for_the_restart() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let seen: std::sync::Arc<std::sync::Mutex<Vec<(u32, u32, u32)>>> = Default::default();
            let input: SourceNode<u32> = scope.batch(|| {
                let input: SourceNode<u32> = effect!(0);
                effect!(async |input| {
                    let made_at: u32 = *input.get();
                    let runs: std::sync::Arc<std::sync::atomic::AtomicU32> = Default::default();
                    effect!(|input| {
                        let run: u32 = runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let record: (u32, u32, u32) = (*made_at, *input.get(), run);
                        seen.lock().unwrap().push(record);
                    });
                    SuspendOnce(false).await;
                });
                input
            });
            scope.batch(|| input.set(1)); // before, during or after the first run
            scope.flush();
            let runs: Vec<(u32, u32, u32)> = seen.lock().unwrap().clone();
            assert!(
                runs.iter().all(|(made_at, value, run): &(u32, u32, u32)| *run == 0 || made_at == value),
                "{runs:?}"
            );
            assert!(runs.contains(&(1, 1, 0)), "{runs:?}");
        });
    });
}

/// An idle old child and its creator are decided in the same step: the child is freed before
/// its task exists, so it never runs (Case AO).
#[test]
fn case_ao_an_idle_old_child_never_runs() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (a, inner_runs): (SourceNode<u32>, SourceNode<u32>) = scope.batch(|| {
                let a: SourceNode<u32> = effect!(0);
                let inner_runs: SourceNode<u32> = effect!(0);
                effect!(|a| {
                    let _ = a.get();
                    effect!(|a| {
                        let _ = a.get();
                        inner_runs.update(|n: &u32| n + 1);
                    });
                });
                (a, inner_runs)
            });
            scope.flush();
            assert_eq!(*inner_runs.get(), 1);
            scope.batch(|| a.set(1));
            scope.flush();
            assert_eq!((*inner_runs.get(), scope.live()), (2, 4));
        });
    });
}

/// A write supersedes a run while it is being polled: what the run created survives until it stops.
#[test]
fn a_superseded_run_keeps_its_creations_until_it_stops() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let created: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
            let lost: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
            let (id, _loader): (SourceNode<u32>, DerivedNode<()>) = scope.batch(|| {
                let id: SourceNode<u32> = effect!(7);
                let k: SourceNode<u32> = effect!(1);
                let sc: ModelScope = scope.clone();
                let loader: DerivedNode<()> = effect!(|id| {
                    if *id.get() == 7 {
                        let before: usize = sc.live();
                        effect!(|k| {
                            let _ = k.get();
                        });
                        created.store(true, Ordering::SeqCst);
                        loom::thread::yield_now(); // the write may land here
                        if sc.live() != before + 1 {
                            lost.store(true, Ordering::SeqCst);
                        }
                    }
                });
                (id, loader)
            });
            wait_for(&created);
            scope.batch(|| id.set(8));
            scope.flush();
            assert!(!lost.load(Ordering::SeqCst), "a run in flight lost a node it created");
            assert_eq!(scope.live(), 3); // the rerun released it
        });
    });
}

/// A node is disposed while its body still creates: a creation nothing holds never runs, and
/// one stored in a held node is seeded, runs, and the scope settles (Case AR).
#[test]
fn case_ar_creations_under_a_disposed_body() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let started: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
            let disposed: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
            let (free_runs, held_runs, slot, body): (
                SourceNode<u32>,
                SourceNode<u32>,
                SourceNode<Option<DerivedNode<()>>>,
                DerivedNode<()>,
            ) = scope.batch(|| {
                let k: SourceNode<u32> = effect!(1);
                let free_runs: SourceNode<u32> = effect!(0);
                let held_runs: SourceNode<u32> = effect!(0);
                let slot: SourceNode<Option<DerivedNode<()>>> = effect!(None);
                let body: DerivedNode<()> = effect!(|k| {
                    let _ = k.get();
                    started.store(true, Ordering::SeqCst);
                    wait_for(disposed);
                    effect!(|k| {
                        let _ = k.get();
                        free_runs.update(|n: &u32| n + 1);
                    });
                    slot.set(Some(effect!(|k| {
                        let _ = k.get();
                        held_runs.update(|n: &u32| n + 1);
                    })));
                });
                (free_runs, held_runs, slot, body)
            });
            wait_for(&started);
            lithia::dispose(&body);
            disposed.store(true, Ordering::SeqCst);
            // The body's write to `slot` is visible before its batch is delivered, so wait for
            // the held creation's run itself; a creation that never ran would spin here.
            while *held_runs.get() == 0 {
                if !model::run_one() {
                    loom::thread::yield_now();
                }
            }
            scope.flush();
            assert!(slot.get().is_some());
            assert_eq!((*free_runs.get(), *held_runs.get()), (0, 1));
        });
    });
}

/// A write supersedes an async block that created a node: once the block has stopped, its
/// creation is released and freed (Case AQ).
#[test]
fn case_aq_a_write_releases_a_superseded_blocks_creations() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let go: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
            let block: SourceNode<u32> = scope.batch(|| {
                effect!(async {
                    effect!(1u32);
                    WaitFor(go.clone()).await;
                    5u32
                })
            });
            model::run_one();
            scope.batch(|| block.set(9));
            go.store(true, Ordering::SeqCst);
            scope.settle(&block);
            assert_eq!((*block.get(), scope.live()), (9, 1));
        });
    });
}
