// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! The validation cases of spec §13 that race a worker.

use super::*;

#[test]
fn case_a_nested_creation() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (array, callcount, _expansion): (SourceNode<Vec<SourceNode<i32>>>, SourceNode<i32>, DerivedNode<()>) =
                scope.batch(|| {
                    let array: SourceNode<Vec<SourceNode<i32>>> = node!(Vec::new());
                    let callcount: SourceNode<i32> = node!(0);
                    let expansion: DerivedNode<()> = node!(|array| {
                        callcount.update(|n: &i32| n + 1);
                        let c: SourceNode<i32> = node!(42);
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
                let a: SourceNode<i32> = node!(1);
                let b: DerivedNode<i32> = node!(|a| *a.get() + 1);
                let c: DerivedNode<i32> = node!(|a| *a.get() * 10);
                let runs: SourceNode<u32> = node!(0);
                let d: DerivedNode<i32> = node!(|b, c| {
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
                let x: SourceNode<i32> = node!(0);
                let runs: SourceNode<u32> = node!(0);
                let clamp: DerivedNode<()> = node!(|x| {
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
                let x: SourceNode<i32> = node!(0);
                let clamp: DerivedNode<()> = node!(|x| if *x.get() > 10 {
                    x.set(10);
                });
                let shown: DerivedNode<i32> = node!(|x| *x.get());
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
                let x: SourceNode<i32> = node!(0);
                let doubled: DerivedNode<i32> = node!(|x| *x.get() * 2);
                let clamp: DerivedNode<()> = node!(|doubled| if *doubled.get() > 20 {
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
                let a: SourceNode<i32> = node!(1);
                let slow: DerivedNode<i32> = node!(|a| {
                    started.store(true, Ordering::SeqCst);
                    wait_for(go); // still running while `m` is created
                    *a.get()
                });
                let holder: SourceNode<Option<DerivedNode<i32>>> = node!(None);
                let m_runs: SourceNode<u32> = node!(0);
                let maker: DerivedNode<()> = node!(|a| {
                    wait_for(started);
                    holder.set(Some(node!(|slow| {
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
                let x: SourceNode<i32> = node!(0);
                let doubled: DerivedNode<i32> = node!(|x| *x.get() * 2);
                let back: DerivedNode<()> = node!(|doubled| x.set(*doubled.get() / 2 + 1));
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
                let a: SourceNode<i32> = node!(1);
                let parity: DerivedNode<i32> = node!(|a| *a.get() % 2, equals = lithia::eq);
                let runs: SourceNode<u32> = node!(0);
                let e: DerivedNode<()> = node!(|parity| runs.update(|n: &u32| n + 1));
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
                    let x: SourceNode<i32> = node!(0);
                    let y: SourceNode<i32> = node!(0);
                    let doubled: DerivedNode<i32> = node!(|x| *x.get() * 2);
                    let clamp: DerivedNode<bool> = node!(|doubled| {
                        let over: bool = *doubled.get() > 20;
                        if over {
                            x.set(10);
                        }
                        over
                    });
                    let shown: DerivedNode<i32> = node!(|clamp, y| *y.get());
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
                let a: SourceNode<i32> = node!(1, equals = lithia::eq);
                let runs: SourceNode<u32> = node!(0);
                let child: DerivedNode<()> = node!(|a| runs.update(|n: &u32| n + 1));
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
                let me: SourceNode<Option<DerivedNode<u32>>> = node!(None);
                let tick: SourceNode<u32> = node!(0, no_equals); // the same value must still reach the node
                let body_runs: SourceNode<u32> = node!(0);
                let child_runs: SourceNode<u32> = node!(0);
                let node: DerivedNode<u32> = node!(
                    |tick| {
                        body_runs.update(|n: &u32| n + 1);
                        if let Some(handle) = &*me.get() {
                            lithia::reflow(handle);
                        }
                        *tick.get()
                    },
                    equals = lithia::eq
                );
                let child: DerivedNode<()> = node!(|node| child_runs.update(|n: &u32| n + 1));
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
/// source is freed with it, and a scoped one stops with its handle (Case J).
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
                let count: SourceNode<i32> = node!(0);
                let runs: SourceNode<u32> = node!(0);
                let local: SourceNode<i32> = node!(0);
                let scoped_runs: SourceNode<u32> = node!(0);
                node!(|count| runs.update(|n: &u32| n + 1));
                node!(|local| runs.update(|n: &u32| n + 1));
                let scoped: DerivedNode<()> = node!(|count| scoped_runs.update(|n: &u32| n + 1)).scoped();
                (count, runs, local, scoped_runs, scoped)
            });
            scope.flush();
            assert_eq!(scope.live(), 7);
            drop(local);
            drop(scoped); // a scoped effect stops when its last handle drops
            scope.flush();
            assert_eq!(scope.live(), 4);
            scope.batch(|| count.set(1));
            scope.flush();
            assert_eq!((*runs.get(), *scoped_runs.get()), (3, 1));
        });
    });
}

#[test]
fn case_e_failure_follows_a_failing_parent() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (n, inv, strict, lenient): (SourceNode<i32>, DerivedNode<i32>, DerivedNode<i32>, DerivedNode<i32>) =
                scope.batch(|| {
                    let n: SourceNode<i32> = node!(1);
                    let inv: DerivedNode<i32> = node!(|n| 100 / *n.get());
                    let strict: DerivedNode<i32> = node!(|inv| *inv.get() + 1, resilience = NONE);
                    let lenient: DerivedNode<i32> = node!(|inv| *inv.get() + 2);
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

/// A deferred write runs after the epoch settles, in a fresh one, and the scope waits for it.
#[test]
fn case_x_defer_feeds_back_once() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (x, runs, _bump): (SourceNode<u32>, SourceNode<u32>, DerivedNode<()>) = scope.batch(|| {
                let x: SourceNode<u32> = node!(0);
                let runs: SourceNode<u32> = node!(0);
                let bump: DerivedNode<()> = node!(|x| {
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

/// An async run released its children at its suspension; a write supersedes it while it is
/// suspended, and the children end on the restarted run's result (Case D, spec §7.3 item 4).
#[test]
fn case_d_a_write_supersedes_a_suspended_run() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (id, name, seen, _view): (SourceNode<u32>, DerivedNode<String>, SourceNode<String>, DerivedNode<()>) =
                scope.batch(|| {
                    let id: SourceNode<u32> = node!(7);
                    let name: DerivedNode<String> = node!(async |id| {
                        let value: u32 = *id.get();
                        SuspendOnce(false).await;
                        format!("user-{value}")
                    });
                    let seen: SourceNode<String> = node!(String::new());
                    let view: DerivedNode<()> = node!(|name| seen.set((*name.get()).clone()));
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
                let slow: SourceNode<u32> = node!(async {
                    SuspendOnce(false).await;
                    1
                });
                let copy: DerivedNode<u32> = node!(|slow| *slow.get());
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
