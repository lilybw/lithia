// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Loom models of the scheduling protocol over the validation cases (spec §11.4 step 8).
//! `RUSTFLAGS="--cfg lithia_loom" cargo test -p lithia-test --test loom --release`; bound: `LOOM_MAX_PREEMPTIONS`.

#![cfg(lithia_loom)]
#![allow(clippy::type_complexity)]

use lithia::__private::model::{self, ModelScope};
use lithia::{DerivedNode, SourceNode, Status, effect};
use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, Ordering};

/// Silences panics the models cause on purpose, which the run wrapper catches.
fn quiet_expected_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let default: Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static> = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info: &std::panic::PanicHookInfo<'_>| {
            let message: String = info.to_string();
            let expected: bool =
                message.contains("attempt to divide by zero") || message.contains("read of a disposed node");
            if !expected {
                default(info);
            }
        }));
    });
}

/// Explores every interleaving of `f` within the preemption bound; prints how many.
fn check(f: impl Fn() + Sync + Send + 'static) {
    quiet_expected_panics();
    let mut builder: loom::model::Builder = loom::model::Builder::new();
    if builder.preemption_bound.is_none() {
        builder.preemption_bound = Some(3);
    }
    builder.max_branches = 1_000_000;
    let executions: std::sync::Arc<std::sync::atomic::AtomicUsize> = Default::default();
    let counter: std::sync::Arc<std::sync::atomic::AtomicUsize> = executions.clone();
    builder.check(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        model::reset();
        f();
    });
    eprintln!("explored {} interleavings", executions.load(std::sync::atomic::Ordering::Relaxed));
}

/// Runs `f` with one worker thread polling runs next to the model's main thread.
fn with_worker(f: impl FnOnce()) {
    let stop: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    let worker: loom::thread::JoinHandle<()> = {
        let stop: Arc<AtomicBool> = stop.clone();
        loom::thread::spawn(move || model::work_until(&stop))
    };
    f();
    stop.store(true, Ordering::SeqCst);
    worker.join().unwrap();
}

/// Spins until `flag` is set, letting loom schedule other threads.
fn wait_for(flag: &AtomicBool) {
    while !flag.load(Ordering::SeqCst) {
        loom::thread::yield_now();
    }
}

#[test]
fn case_a_nested_creation() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (array, callcount, _expansion): (SourceNode<Vec<SourceNode<i32>>>, SourceNode<i32>, DerivedNode<()>) =
                scope.batch(|| {
                    let array: SourceNode<Vec<SourceNode<i32>>> = effect!(Vec::new());
                    let callcount: SourceNode<i32> = effect!(0);
                    let expansion: DerivedNode<()> = effect!(|array| {
                        callcount.update(|n: &i32| n + 1);
                        let c: SourceNode<i32> = effect!(42);
                        array.update(|old: &Vec<SourceNode<i32>>| {
                            let mut v: Vec<SourceNode<i32>> = old.clone();
                            v.push(c.clone());
                            v
                        });
                    });
                    array.set(Vec::new());
                    (array, callcount, expansion)
                });
            scope.flush();
            assert_eq!(*callcount.get(), 1);
            assert_eq!(array.get().len(), 1);
            assert!(model::quiet(&array));
        });
    });
}

#[test]
fn case_b_diamond() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (a, d, runs): (SourceNode<i32>, DerivedNode<i32>, SourceNode<u32>) = scope.batch(|| {
                let a: SourceNode<i32> = effect!(1);
                let b: DerivedNode<i32> = effect!(|a| *a.get() + 1);
                let c: DerivedNode<i32> = effect!(|a| *a.get() * 10);
                let runs: SourceNode<u32> = effect!(0);
                let d: DerivedNode<i32> = effect!(|b, c| {
                    runs.update(|n: &u32| n + 1);
                    *b.get() + *c.get()
                });
                (a, d, runs)
            });
            scope.flush();
            assert_eq!((*d.get(), *runs.get()), (12, 1));
            scope.batch(|| a.set(2));
            scope.flush();
            assert_eq!((*d.get(), *runs.get()), (23, 2));
            assert!(model::quiet(&a));
        });
    });
}

#[test]
fn case_c_clamp_cycle() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (x, runs, _clamp): (SourceNode<i32>, SourceNode<u32>, DerivedNode<()>) = scope.batch(|| {
                let x: SourceNode<i32> = effect!(0);
                let runs: SourceNode<u32> = effect!(0);
                let clamp: DerivedNode<()> = effect!(|x| {
                    runs.update(|n: &u32| n + 1);
                    if *x.get() > 10 {
                        x.set(10);
                    }
                });
                x.set(15);
                (x, runs, clamp)
            });
            scope.flush();
            assert_eq!((*x.get(), *runs.get()), (10, 1));
            assert!(model::quiet(&x));
        });
    });
}

#[test]
fn case_h_cycle_sibling() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (x, shown, _clamp): (SourceNode<i32>, DerivedNode<i32>, DerivedNode<()>) = scope.batch(|| {
                let x: SourceNode<i32> = effect!(0);
                let clamp: DerivedNode<()> = effect!(|x| if *x.get() > 10 {
                    x.set(10);
                });
                let shown: DerivedNode<i32> = effect!(|x| *x.get());
                x.set(15);
                (x, shown, clamp)
            });
            scope.flush();
            assert_eq!(*shown.get(), 10);
            assert!(model::quiet(&x));
        });
    });
}

#[test]
fn case_m_clamp_through_memo() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (x, doubled, _clamp): (SourceNode<i32>, DerivedNode<i32>, DerivedNode<()>) = scope.batch(|| {
                let x: SourceNode<i32> = effect!(0);
                let doubled: DerivedNode<i32> = effect!(|x| *x.get() * 2);
                let clamp: DerivedNode<()> = effect!(|doubled| if *doubled.get() > 20 {
                    x.set(10);
                });
                x.set(15);
                (x, doubled, clamp)
            });
            scope.flush();
            assert_eq!((*x.get(), *doubled.get()), (10, 20));
            assert!(model::quiet(&x));
        });
    });
}

#[test]
fn case_n_created_mid_propagation() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let started: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
            let go: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
            let (holder, m_runs, _slow, _maker): (
                SourceNode<Option<DerivedNode<i32>>>,
                SourceNode<u32>,
                DerivedNode<i32>,
                DerivedNode<()>,
            ) = scope.batch(|| {
                let a: SourceNode<i32> = effect!(1);
                let slow: DerivedNode<i32> = effect!(|a| {
                    started.store(true, Ordering::SeqCst);
                    wait_for(go); // still running while `m` is created
                    *a.get()
                });
                let holder: SourceNode<Option<DerivedNode<i32>>> = effect!(None);
                let m_runs: SourceNode<u32> = effect!(0);
                let maker: DerivedNode<()> = effect!(|a| {
                    wait_for(started);
                    holder.set(Some(effect!(|slow| {
                        m_runs.update(|n: &u32| n + 1);
                        *slow.get() + 1
                    })));
                    go.store(true, Ordering::SeqCst);
                });
                (holder, m_runs, slow, maker)
            });
            scope.flush();
            let m: DerivedNode<i32> = (*holder.get()).clone().expect("set by maker");
            assert_eq!(*m.get(), 2);
            assert_eq!(*m_runs.get(), 1, "m's entry waited on slow's in-flight run, so m ran once");
            holder.set(None);
        });
    });
}

#[test]
fn case_p_unconditional_writeback() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (x, doubled, _back): (SourceNode<i32>, DerivedNode<i32>, DerivedNode<()>) = scope.batch(|| {
                let x: SourceNode<i32> = effect!(0);
                let doubled: DerivedNode<i32> = effect!(|x| *x.get() * 2);
                let back: DerivedNode<()> = effect!(|doubled| x.set(*doubled.get() / 2 + 1));
                x.set(5);
                (x, doubled, back)
            });
            scope.flush();
            assert_eq!((*x.get(), *doubled.get()), (6, 12));
            assert!(model::quiet(&x));
        });
    });
}

#[test]
fn case_r_equal_parent_stops_propagation() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (a, runs, _e): (SourceNode<i32>, SourceNode<u32>, DerivedNode<()>) = scope.batch(|| {
                let a: SourceNode<i32> = effect!(1);
                let parity: DerivedNode<i32> = effect!(|a| *a.get() % 2, equals = lithia::eq);
                let runs: SourceNode<u32> = effect!(0);
                let e: DerivedNode<()> = effect!(|parity| runs.update(|n: &u32| n + 1));
                (a, runs, e)
            });
            scope.flush();
            scope.batch(|| a.set(3));
            scope.flush();
            assert_eq!(*runs.get(), 1);
            assert!(model::quiet(&a));
        });
    });
}

#[test]
fn case_t_cut_writer_releases_children() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (x, y, shown, _clamp): (SourceNode<i32>, SourceNode<i32>, DerivedNode<i32>, DerivedNode<bool>) = scope
                .batch(|| {
                    let x: SourceNode<i32> = effect!(0);
                    let y: SourceNode<i32> = effect!(0);
                    let doubled: DerivedNode<i32> = effect!(|x| *x.get() * 2);
                    let clamp: DerivedNode<bool> = effect!(|doubled| {
                        let over: bool = *doubled.get() > 20;
                        if over {
                            x.set(10);
                        }
                        over
                    });
                    let shown: DerivedNode<i32> = effect!(|clamp, y| *y.get());
                    x.set(15);
                    (x, y, shown, clamp)
                });
            scope.flush();
            scope.batch(|| y.set(1));
            scope.flush();
            assert_eq!(*shown.get(), 1);
            assert!(model::quiet(&x) && model::quiet(&y));
        });
    });
}

#[test]
fn case_v_reflow() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (a, runs, _child): (SourceNode<i32>, SourceNode<u32>, DerivedNode<()>) = scope.batch(|| {
                let a: SourceNode<i32> = effect!(1, equals = lithia::eq);
                let runs: SourceNode<u32> = effect!(0);
                let child: DerivedNode<()> = effect!(|a| runs.update(|n: &u32| n + 1));
                (a, runs, child)
            });
            scope.flush();
            scope.batch(|| a.set(1));
            scope.flush();
            assert_eq!(*runs.get(), 1);
            scope.batch(|| lithia::reflow(&a));
            scope.flush();
            assert_eq!(*runs.get(), 2);
        });
    });
}

#[test]
fn case_w_reflow_from_its_own_body() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (me, tick, body_runs, child_runs, node, _child): (
                SourceNode<Option<DerivedNode<u32>>>,
                SourceNode<u32>,
                SourceNode<u32>,
                SourceNode<u32>,
                DerivedNode<u32>,
                DerivedNode<()>,
            ) = scope.batch(|| {
                let me: SourceNode<Option<DerivedNode<u32>>> = effect!(None);
                let tick: SourceNode<u32> = effect!(0);
                let body_runs: SourceNode<u32> = effect!(0);
                let child_runs: SourceNode<u32> = effect!(0);
                let node: DerivedNode<u32> = effect!(
                    |tick| {
                        body_runs.update(|n: &u32| n + 1);
                        if let Some(handle) = &*me.get() {
                            lithia::reflow(handle);
                        }
                        *tick.get()
                    },
                    equals = lithia::eq
                );
                let child: DerivedNode<()> = effect!(|node| child_runs.update(|n: &u32| n + 1));
                (me, tick, body_runs, child_runs, node, child)
            });
            scope.flush();
            scope.batch(|| {
                me.set(Some(node.clone()));
                tick.set(0);
            });
            scope.flush();
            assert_eq!((*body_runs.get(), *child_runs.get()), (2, 2));
            me.set(None);
        });
    });
}

/// An effect lives while its parent can change: one on `count` keeps running, one on a dropped
/// source is freed with it, and a scoped one stops with its handle (Case J as rewritten for v0.3.0).
#[test]
fn case_j_an_effect_lives_while_its_parent_can_change() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (count, runs, local, scoped_runs, scoped): (
                SourceNode<i32>,
                SourceNode<u32>,
                SourceNode<i32>,
                SourceNode<u32>,
                DerivedNode<()>,
            ) = scope.batch(|| {
                let count: SourceNode<i32> = effect!(0);
                let runs: SourceNode<u32> = effect!(0);
                let local: SourceNode<i32> = effect!(0);
                let scoped_runs: SourceNode<u32> = effect!(0);
                effect!(|count| runs.update(|n: &u32| n + 1));
                effect!(|local| runs.update(|n: &u32| n + 1));
                let scoped: DerivedNode<()> = effect!(|count| scoped_runs.update(|n: &u32| n + 1)).scoped();
                (count, runs, local, scoped_runs, scoped)
            });
            scope.flush();
            assert_eq!(scope.live(), 7);
            drop(local);
            drop(scoped); // a scoped effect stops when its last handle drops, as in v0.2.5
            scope.flush();
            assert_eq!(scope.live(), 4);
            scope.batch(|| count.set(1));
            scope.flush();
            assert_eq!((*runs.get(), *scoped_runs.get()), (3, 1));
        });
    });
}

#[test]
fn case_e_fragility_follows_a_failing_parent() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (n, inv, strict, lenient): (SourceNode<i32>, DerivedNode<i32>, DerivedNode<i32>, DerivedNode<i32>) =
                scope.batch(|| {
                    let n: SourceNode<i32> = effect!(1);
                    let inv: DerivedNode<i32> = effect!(|n| 100 / *n.get());
                    let strict: DerivedNode<i32> = effect!(|inv| *inv.get() + 1, fragility = FRAGILE);
                    let lenient: DerivedNode<i32> = effect!(|inv| *inv.get() + 2);
                    (n, inv, strict, lenient)
                });
            scope.flush();
            scope.batch(|| n.set(0));
            scope.flush();
            assert!(matches!(lithia::status(&inv), Status::Failed(_)));
            assert!(matches!(lithia::status(&strict), Status::Failed(_)));
            assert!(matches!(lithia::status(&lenient), Status::Ready));
            scope.batch(|| n.set(4));
            scope.flush();
            assert_eq!((*inv.get(), *strict.get()), (25, 26));
            assert!(matches!(lithia::status(&strict), Status::Ready));
        });
    });
}

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

/// Two runs serving one epoch write the same source, on two threads: one's release may reach the
/// source's child while the other still frames the child's region, so the region waits on one
/// release per write, and every node below settles (found by the TLA+ model, `docs/tla`).
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

/// A run publishes its value and is superseded before its poll's batch is delivered; the run that
/// replaces it computes an equal value. The published change still reaches the children
/// (found by the TLA+ model, `docs/tla`).
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

/// Disposing a node while its run may be in flight settles the scope and leaves a ghost.
#[test]
fn dispose_during_a_run_settles() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (a, d, tail): (SourceNode<i32>, DerivedNode<i32>, DerivedNode<i32>) = scope.batch(|| {
                let a: SourceNode<i32> = effect!(1);
                let d: DerivedNode<i32> = effect!(|a| *a.get() + 1);
                let tail: DerivedNode<i32> = effect!(|d| *d.get() * 10);
                (a, d, tail)
            });
            scope.batch(|| a.set(2));
            model::run_one();
            lithia::dispose(&d);
            scope.flush();
            assert!(matches!(lithia::status(&d), Status::Ghost));
            assert!(matches!(lithia::status(&tail), Status::Ghost));
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

/// A deferred write runs after the epoch settles, in a fresh one, and the scope waits for it.
#[test]
fn case_x_defer_feeds_back_once() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (x, runs, _bump): (SourceNode<u32>, SourceNode<u32>, DerivedNode<()>) = scope.batch(|| {
                let x: SourceNode<u32> = effect!(0);
                let runs: SourceNode<u32> = effect!(0);
                let bump: DerivedNode<()> = effect!(|x| {
                    runs.update(|n: &u32| n + 1);
                    if *x.get() == 0 {
                        let later: SourceNode<u32> = x.clone();
                        lithia::defer(move || later.set(1));
                    }
                });
                (x, runs, bump)
            });
            scope.flush();
            assert_eq!((*x.get(), *runs.get()), (1, 2));
        });
    });
}

/// Suspends once, then completes: an async body's `.await` on something not yet ready.
struct SuspendOnce(bool);

impl std::future::Future for SuspendOnce {
    type Output = ();

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        if self.0 {
            return std::task::Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    }
}

/// An async run released its children at its suspension; a write supersedes it while it is
/// suspended, and the children end on the restarted run's result (Case D, spec §7.3 item 4).
#[test]
fn case_d_a_write_supersedes_a_suspended_run() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (id, name, seen, _view): (SourceNode<u32>, DerivedNode<String>, SourceNode<String>, DerivedNode<()>) =
                scope.batch(|| {
                    let id: SourceNode<u32> = effect!(7);
                    let name: DerivedNode<String> = effect!(async |id| {
                        let value: u32 = *id.get();
                        SuspendOnce(false).await;
                        format!("user-{value}")
                    });
                    let seen: SourceNode<String> = effect!(String::new());
                    let view: DerivedNode<()> = effect!(|name| seen.set((*name.get()).clone()));
                    (id, name, seen, view)
                });
            model::run_one();
            scope.batch(|| id.set(8));
            scope.settle(&id); // flush never waits for async results; settled does
            assert_eq!(name.get().as_str(), "user-8");
            assert_eq!(seen.get().as_str(), "user-8");
            assert!(model::quiet(&id));
        });
    });
}

/// A write to an async source supersedes its block, before or after it starts (Case G).
#[test]
fn case_g_a_write_stops_an_async_source() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (slow, copy): (SourceNode<u32>, DerivedNode<u32>) = scope.batch(|| {
                let slow: SourceNode<u32> = effect!(async {
                    SuspendOnce(false).await;
                    1
                });
                let copy: DerivedNode<u32> = effect!(|slow| *slow.get());
                (slow, copy)
            });
            model::run_one();
            scope.batch(|| slow.set(2));
            scope.settle(&slow);
            assert_eq!((*slow.get(), *copy.get()), (2, 2));
            assert!(matches!(lithia::status(&slow), Status::Ready));
            assert!(model::quiet(&slow));
        });
    });
}

/// Pending until `flag` is set, yielding to loom while it waits.
struct WaitFor(Arc<AtomicBool>);

impl std::future::Future for WaitFor {
    type Output = ();

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        if self.0.load(Ordering::SeqCst) {
            return std::task::Poll::Ready(());
        }
        loom::thread::yield_now();
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    }
}

/// A last-handle drop on one thread races a top-level creation on the same node on another;
/// both nodes are freed whichever lands first (spec §10.1, §11.4 step 1).
#[test]
fn a_last_handle_drop_races_a_creation() {
    check(|| {
        let scope: ModelScope = ModelScope::new();
        let (n, seen): (SourceNode<u32>, SourceNode<u32>) = scope.batch(|| (effect!(1), effect!(0)));
        let (other, seen_there): (SourceNode<u32>, SourceNode<u32>) = (n.clone(), seen.clone());
        let creator: loom::thread::JoinHandle<ModelScope> = loom::thread::spawn(move || {
            let there: ModelScope = ModelScope::new();
            let made: DerivedNode<()> = there.batch(|| effect!(|other| seen_there.set(*other.get())));
            drop(other);
            drop(made);
            there
        });
        drop(n);
        let there: ModelScope = creator.join().unwrap();
        scope.flush();
        there.flush();
        assert_eq!((scope.live(), there.live()), (1, 0));
    });
}

/// A node created while its only parent is disposed ends a ghost: disposal frees it, finds it
/// linked when the parent closes, or it finds the parent closed itself.
#[test]
fn a_creation_races_the_disposal_of_its_only_parent() {
    check(|| {
        let scope: ModelScope = ModelScope::new();
        let p: SourceNode<u32> = scope.batch(|| effect!(1));
        let parent: SourceNode<u32> = p.clone();
        let creator: loom::thread::JoinHandle<(ModelScope, DerivedNode<u32>)> = loom::thread::spawn(move || {
            let there: ModelScope = ModelScope::new();
            let orphan: DerivedNode<u32> = there.batch(|| effect!(|parent| *parent.get()));
            (there, orphan)
        });
        lithia::dispose(&p);
        let (there, orphan): (ModelScope, DerivedNode<u32>) = creator.join().unwrap();
        scope.flush();
        there.flush();
        assert!(matches!(lithia::status(&orphan), Status::Ghost));
    });
}

/// A fragile node created, and run on the worker, while one of its parents is disposed ends
/// failed: it reruns as a surviving child, or its first run already sees the ghost.
#[test]
fn a_creation_races_the_disposal_of_one_parent() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (p, k): (SourceNode<u32>, SourceNode<u32>) = scope.batch(|| (effect!(1), effect!(2)));
            let (parent, live): (SourceNode<u32>, SourceNode<u32>) = (p.clone(), k.clone());
            let creator: loom::thread::JoinHandle<(ModelScope, DerivedNode<u32>)> = loom::thread::spawn(move || {
                let there: ModelScope = ModelScope::new();
                let strict: DerivedNode<u32> = there.batch(|| effect!(|parent, live| *live.get(), fragility = FRAGILE));
                (there, strict)
            });
            lithia::dispose(&p);
            let (there, strict): (ModelScope, DerivedNode<u32>) = creator.join().unwrap();
            scope.flush();
            there.flush();
            let status: Status = lithia::status(&strict);
            assert!(matches!(status, Status::Failed(_)), "{status:?}");
            drop(k);
        });
    });
}

/// A handle a `move` block moves out drops while the block's lease ends on the worker: it is
/// counted once either way, so the watcher of its node goes with it.
#[test]
fn a_moved_out_handle_drops_while_its_lease_ends() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let slot: std::sync::Arc<std::sync::Mutex<Option<SourceNode<u32>>>> = Default::default();
            let out: std::sync::Arc<std::sync::Mutex<Option<SourceNode<u32>>>> = slot.clone();
            let block: SourceNode<()> = scope.batch(|| {
                let t: SourceNode<u32> = effect!(1);
                effect!(|t| {
                    let _ = t.get();
                });
                effect!(async move {
                    *out.lock().unwrap() = Some(t);
                })
            });
            let moved: SourceNode<u32> = loop {
                if let Some(handle) = slot.lock().unwrap().take() {
                    break handle;
                }
                loom::thread::yield_now();
            };
            drop(moved);
            scope.flush();
            drop(block);
            scope.flush();
            assert_eq!(scope.live(), 0);
        });
    });
}

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

/// A write supersedes an async creator's run that created a child: the child waits for the
/// restart, which lets go of it. Its first run may already be under way, reading what is current,
/// but it never runs again on the input that superseded its creator.
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

/// Two threads write one source at once; a `WHOLE_EPOCHS` join of its two paths holds its runs
/// while an epoch that reached it is on its way, and still ends quiet on the final value. A write
/// is readable before its epoch is delivered, so a run may read it early; its epoch reruns the join.
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

/// A parent's and its child's last handles drop at once: both are examined, and both freed.
#[test]
fn a_parent_and_its_child_drop_their_last_handles_at_once() {
    check(|| {
        let scope: ModelScope = ModelScope::new();
        let (p, c): (SourceNode<u32>, DerivedNode<u32>) = scope.batch(|| {
            let p: SourceNode<u32> = effect!(1);
            let c: DerivedNode<u32> = effect!(|p| *p.get());
            (p, c)
        });
        scope.flush();
        let other: loom::thread::JoinHandle<()> = loom::thread::spawn(move || drop(c));
        drop(p);
        other.join().unwrap();
        assert_eq!(scope.live(), 0);
    });
}

/// A body anchors `m` for a moment on the worker while a sweep from upstream runs over `m`:
/// `m`'s watcher is freed either way.
#[test]
fn anchors_cross_zero_while_a_sweep_runs() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (s, trigger): (SourceNode<u32>, SourceNode<u32>) = scope.batch(|| {
                let s: SourceNode<u32> = effect!(1);
                let trigger: SourceNode<u32> = effect!(0);
                let m: DerivedNode<u32> = effect!(|s| *s.get());
                effect!(|m| {
                    let _ = m.get();
                });
                effect!(|trigger| {
                    let _ = trigger.get();
                    let _ = m.accessor(); // a temporary anchor on `m`
                });
                (s, trigger)
            });
            scope.flush();
            assert_eq!(scope.live(), 5);
            scope.batch(|| trigger.set(1));
            drop(s);
            scope.flush();
            assert_eq!(scope.live(), 4); // the trigger's effect still holds `m`, and `m` holds `s`
        });
    });
}

/// One sweep's let-go races another sweep's walk over the same nodes; all are freed.
#[test]
fn a_let_go_races_another_sweeps_walk() {
    check(|| {
        let scope: ModelScope = ModelScope::new();
        let (s, u): (SourceNode<u32>, SourceNode<u32>) = scope.batch(|| {
            let s: SourceNode<u32> = effect!(1);
            let u: SourceNode<u32> = effect!(2);
            let m: DerivedNode<u32> = effect!(|s| *s.get());
            effect!(|m, u| {
                let _ = (m.get(), u.get());
            });
            (s, u)
        });
        scope.flush();
        let other: loom::thread::JoinHandle<()> = loom::thread::spawn(move || drop(u));
        drop(s);
        other.join().unwrap();
        scope.flush();
        assert_eq!(scope.live(), 0);
    });
}

/// A sweep starts inside an `update` closure, with the node's write lock held, and its drops
/// raise further starting points for the same operation.
#[test]
fn a_sweep_inside_an_update_closure() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (x, held): (SourceNode<u32>, std::sync::Mutex<Option<SourceNode<u32>>>) = scope.batch(|| {
                let x: SourceNode<u32> = effect!(0);
                let t: SourceNode<u32> = effect!(1);
                effect!(|x| {
                    let _ = x.get();
                });
                effect!(|t| {
                    let _ = t.get();
                });
                (x, std::sync::Mutex::new(Some(t)))
            });
            scope.flush();
            scope.batch(|| {
                x.update(|v: &u32| {
                    drop(held.lock().unwrap().take());
                    v + 1
                })
            });
            scope.flush();
            assert_eq!(scope.live(), 2);
        });
    });
}

/// A sweep copies a fragile child's parent list while the worker decides that child's
/// fragility, which takes the two locks the other way round; neither waits on the other.
#[test]
fn a_sweep_alongside_a_fragile_decision() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (n, k): (SourceNode<i32>, SourceNode<i32>) = scope.batch(|| {
                let n: SourceNode<i32> = effect!(1);
                let k: SourceNode<i32> = effect!(1);
                let inv: DerivedNode<i32> = effect!(|n| 100 / *n.get());
                effect!(|inv, k| *inv.get() + *k.get(), fragility = FRAGILE);
                drop(inv);
                (n, k)
            });
            scope.flush();
            scope.batch(|| n.set(0));
            drop(k); // a sweep from `k` over the fragile child
            scope.flush();
            assert_eq!(scope.live(), 4); // the child still hangs off `n` through `inv`
        });
    });
}

/// A `move` body takes the only handle to `b` while another thread's sweep runs over `b`'s
/// watcher: the handle's count drops only once the body's writer edge exists (Case AS).
#[test]
fn case_as_a_move_body_takes_the_only_handle() {
    check(|| {
        let scope: ModelScope = ModelScope::new();
        let (a, b, k, seen): (SourceNode<u32>, SourceNode<u32>, SourceNode<u32>, SourceNode<u32>) = scope.batch(|| {
            let a: SourceNode<u32> = effect!(1);
            let b: SourceNode<u32> = effect!(0);
            let k: SourceNode<u32> = effect!(0);
            let seen: SourceNode<u32> = effect!(0);
            effect!(|b, k| {
                let _ = k.get();
                seen.set(*b.get());
            });
            (a, b, k, seen)
        });
        scope.flush();
        let other: loom::thread::JoinHandle<()> = loom::thread::spawn(move || drop(k));
        let from_a: SourceNode<u32> = a.clone();
        scope.batch(move || {
            effect!(move |from_a| b.set(*from_a.get() * 2));
        });
        other.join().unwrap();
        scope.flush();
        assert_eq!((scope.live(), *seen.get()), (6, 2));
        scope.batch(|| a.set(3));
        scope.flush();
        assert_eq!(*seen.get(), 6);
    });
}

/// A scoped node's last handle drops while a sweep from upstream runs over it: the node and
/// the effect hanging off it are freed, though the effect has a second, long-lived parent (Case AT).
#[test]
fn case_at_a_scoped_nodes_last_handle_races_an_upstream_sweep() {
    check(|| {
        let scope: ModelScope = ModelScope::new();
        let (rates, input, price): (SourceNode<u32>, SourceNode<u32>, DerivedNode<u32>) = scope.batch(|| {
            let rates: SourceNode<u32> = effect!(2);
            let input: SourceNode<u32> = effect!(3);
            let price: DerivedNode<u32> = effect!(|input, rates| *input.get() * *rates.get()).scoped();
            effect!(|price, rates| {
                let _ = (price.get(), rates.get());
            });
            (rates, input, price)
        });
        scope.flush();
        let other: loom::thread::JoinHandle<()> = loom::thread::spawn(move || drop(price));
        drop(input);
        other.join().unwrap();
        scope.flush();
        assert_eq!(scope.live(), 1);
        drop(rates);
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
