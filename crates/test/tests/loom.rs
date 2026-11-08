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
                    wait_for(&go); // still running while `m` is created
                    *a.get()
                });
                let holder: SourceNode<Option<DerivedNode<i32>>> = effect!(None);
                let m_runs: SourceNode<u32> = effect!(0);
                let maker: DerivedNode<()> = effect!(|a| {
                    wait_for(&started);
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

#[test]
fn case_j_a_dropped_effect_stops() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (count, kept_runs, scoped_runs, _kept, scoped): (
                SourceNode<i32>,
                SourceNode<u32>,
                SourceNode<u32>,
                DerivedNode<()>,
                DerivedNode<()>,
            ) = scope.batch(|| {
                let count: SourceNode<i32> = effect!(0);
                let kept_runs: SourceNode<u32> = effect!(0);
                let scoped_runs: SourceNode<u32> = effect!(0);
                let kept: DerivedNode<()> = effect!(|count| kept_runs.update(|n: &u32| n + 1));
                let scoped: DerivedNode<()> = effect!(|count| scoped_runs.update(|n: &u32| n + 1));
                (count, kept_runs, scoped_runs, kept, scoped)
            });
            scope.flush();
            drop(scoped);
            scope.batch(|| count.set(1));
            scope.flush();
            assert_eq!((*kept_runs.get(), *scoped_runs.get()), (2, 1));
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
