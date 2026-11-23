// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Writes, runs and epochs racing each other (spec §7, §8).

use super::*;

/// Two scopes write the two inputs of one join concurrently: each settles, and once both have,
/// the join matches its inputs (eventual consistency between concurrent epochs, spec §8).
#[test]
fn concurrent_outside_writes_converge() {
    check(|| {
        let scope: ModelScope = ModelScope::new();
        let (a, b, sum): (SourceNode<i32>, SourceNode<i32>, DerivedNode<i32>) = scope.batch(|| {
            let a: SourceNode<i32> = effect!(0);
            let b: SourceNode<i32> = effect!(0);
            let sum: DerivedNode<i32> = effect!(|a, b| *a.get() + *b.get());
            (a, b, sum)
        });
        scope.flush();
        let other: loom::thread::JoinHandle<()> = {
            let b: SourceNode<i32> = b.clone();
            loom::thread::spawn(move || {
                let scope: ModelScope = ModelScope::new();
                scope.batch(|| b.set(20));
                scope.flush();
            })
        };
        scope.batch(|| a.set(1));
        scope.flush();
        other.join().unwrap();
        scope.flush();
        while !model::quiet(&sum) {
            if !model::run_one() {
                loom::thread::yield_now();
            }
        }
        assert_eq!(*sum.get(), 21);
    });
}

/// A write lands while a run of its child is in flight: the run either saw it or is
/// superseded, so the child never ends on a stale value (clock rules, spec §11.4 step 5).
#[test]
fn a_write_during_a_run_is_never_lost() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (a, doubled): (SourceNode<i32>, DerivedNode<i32>) = scope.batch(|| {
                let a: SourceNode<i32> = effect!(1);
                let doubled: DerivedNode<i32> = effect!(|a| *a.get() * 2);
                (a, doubled)
            });
            scope.batch(|| a.set(2));
            scope.flush();
            assert_eq!(*doubled.get(), 4);
            assert!(model::quiet(&a));
        });
    });
}

/// A chain runs on the fast path on the worker while a second write frames it again: whichever
/// run a write supersedes, every node ends on the last value (spec §11.4, step 4).
#[test]
fn an_inline_chain_races_a_second_write() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (a, m1, m2): (SourceNode<i32>, DerivedNode<i32>, DerivedNode<i32>) = scope.batch(|| {
                let a: SourceNode<i32> = effect!(1);
                let m1: DerivedNode<i32> = effect!(|a| *a.get() + 1);
                let m2: DerivedNode<i32> = effect!(|m1| *m1.get() * 10);
                (a, m1, m2)
            });
            scope.batch(|| a.set(2));
            scope.batch(|| a.set(3));
            scope.flush();
            assert_eq!(*m1.get(), 4);
            assert_eq!(*m2.get(), 40);
            assert!(model::quiet(&m2));
        });
    });
}

/// Two runs serving one epoch write one source on two threads: the source's region waits on one
/// release per write, so every node below settles (found by TLC, `docs/tla`).
#[test]
fn two_runs_write_one_source_in_one_epoch() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (a, f): (SourceNode<i32>, DerivedNode<i32>) = scope.batch(|| {
                let a: SourceNode<i32> = effect!(0);
                let t: SourceNode<i32> = effect!(0);
                let _b: DerivedNode<()> = effect!(|a| t.set(*a.get()));
                let _c: DerivedNode<()> = effect!(|a| t.set(*a.get()));
                let d: DerivedNode<i32> = effect!(|t| *t.get() + 1);
                let f: DerivedNode<i32> = effect!(|d| *d.get() * 10);
                (a, f)
            });
            scope.flush();
            scope.batch(|| a.set(1));
            scope.flush();
            assert_eq!(*f.get(), 20);
            assert!(model::quiet(&a));
        });
    });
}

/// A run published its value before it was superseded, and its successor computes an equal one:
/// the published change still reaches the children (found by TLC, `docs/tla`).
#[test]
fn a_change_published_before_a_supersede_reaches_the_children() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (a, c): (SourceNode<i32>, DerivedNode<i32>) = scope.batch(|| {
                let a: SourceNode<i32> = effect!(0);
                let b: DerivedNode<i32> = effect!(|a| *a.get() % 2, equals = lithia::eq);
                let c: DerivedNode<i32> = effect!(|b| *b.get() + 100);
                (a, c)
            });
            scope.flush();
            scope.batch(|| a.set(1));
            scope.batch(|| a.set(3));
            scope.flush();
            assert_eq!(*c.get(), 101);
            assert!(model::quiet(&a));
        });
    });
}

/// A synchronous generator's items each propagate; its child ends on the last one.
#[test]
fn a_generator_child_ends_on_the_last_item() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (tick, last, _log): (DerivedNode<u32>, SourceNode<u32>, DerivedNode<()>) = scope.batch(|| {
                let count_to: SourceNode<u32> = effect!(2);
                let tick: DerivedNode<u32> = effect!(|count_to| {
                    for i in 1..=*count_to.get() {
                        yield i;
                    }
                });
                let last: SourceNode<u32> = effect!(0);
                let log: DerivedNode<()> = effect!(|tick| last.set(*tick.get()));
                (tick, last, log)
            });
            scope.flush();
            assert_eq!((*tick.get(), *last.get()), (2, 2));
            assert!(model::quiet(&tick));
        });
    });
}

/// Writes land while an async run is suspended: one may supersede it, and a later one waits for
/// the replacement's item instead of superseding it again. The node ends on the last input.
#[test]
fn a_replacement_run_is_not_replaced_before_its_item() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (input, slow): (SourceNode<u32>, DerivedNode<u32>) = scope.batch(|| {
                let input: SourceNode<u32> = effect!(0);
                let slow: DerivedNode<u32> = effect!(async |input| {
                    let value: u32 = *input.get();
                    SuspendOnce(false).await;
                    value * 10
                });
                (input, slow)
            });
            scope.batch(|| input.set(1));
            scope.batch(|| input.set(2));
            scope.settle(&slow);
            assert_eq!(*slow.get(), 20);
        });
    });
}

/// A writer cut on its own write reruns once the epoch has settled, wherever its runs land: it
/// ends on the settled value and writes nothing more (spec §7.4).
#[test]
fn a_cut_writer_reruns_after_its_epoch() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (x, over): (SourceNode<i32>, DerivedNode<bool>) = scope.batch(|| {
                let x: SourceNode<i32> = effect!(0);
                let doubled: DerivedNode<i32> = effect!(|x| *x.get() * 2);
                let over: DerivedNode<bool> = effect!(|doubled| {
                    let over: bool = *doubled.get() > 20;
                    if over {
                        x.set(10);
                    }
                    over
                });
                (x, over)
            });
            scope.flush();
            scope.batch(|| x.set(15));
            scope.flush();
            assert_eq!((*x.get(), *over.get()), (10, false));
            assert!(model::quiet(&x));
        });
    });
}

/// Two threads write one source at once: a `WHOLE_EPOCHS` join of its two paths holds its runs
/// while an epoch is on its way to it, and ends on the final value.
#[test]
fn a_whole_epochs_join_converges_under_concurrent_writes() {
    check(|| {
        let scope: ModelScope = ModelScope::new();
        let seen: std::sync::Arc<std::sync::Mutex<Vec<(i32, i32)>>> = Default::default();
        let (s, join): (SourceNode<i32>, DerivedNode<()>) = scope.batch(|| {
            let s: SourceNode<i32> = effect!(0);
            let ones: DerivedNode<i32> = effect!(|s| *s.get());
            let tens: DerivedNode<i32> = effect!(|s| *s.get() * 10);
            let join: DerivedNode<()> = effect!(
                |ones, tens| {
                    let pair: (i32, i32) = (*ones.get(), *tens.get());
                    seen.lock().unwrap().push(pair);
                },
                flags = WHOLE_EPOCHS
            );
            (s, join)
        });
        scope.flush();
        let other: loom::thread::JoinHandle<()> = {
            let s: SourceNode<i32> = s.clone();
            loom::thread::spawn(move || {
                let scope: ModelScope = ModelScope::new();
                scope.batch(|| s.set(2));
                scope.flush();
            })
        };
        scope.batch(|| s.set(1));
        scope.flush();
        other.join().unwrap();
        while !model::quiet(&join) {
            if !model::run_one() {
                loom::thread::yield_now();
            }
        }
        let reads: Vec<(i32, i32)> = seen.lock().unwrap().clone();
        let last: (i32, i32) = *reads.last().unwrap();
        assert_eq!(last, (*s.get(), *s.get() * 10), "{reads:?}");
    });
}
