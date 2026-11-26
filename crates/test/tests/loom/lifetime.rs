// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Handles, disposal, leases and sweeps racing runs (spec §10).

use super::*;

/// Disposing a node while its run may be in flight settles the scope and leaves a ghost.
#[test]
fn dispose_during_a_run_settles() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (a, d, tail): (SourceNode<i32>, DerivedNode<i32>, DerivedNode<i32>) = scope.batch(|| {
                let a: SourceNode<i32> = node!(1);
                let d: DerivedNode<i32> = node!(|a| *a.get() + 1);
                let tail: DerivedNode<i32> = node!(|d| *d.get() * 10);
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

/// A write frames through a node another thread disposes of: the ghost's retire may release its
/// surviving child before the write's walk reaches the child, which then skips the ghost (spec §10.4).
#[test]
fn a_write_frames_through_a_node_being_disposed() {
    check(|| {
        let scope: ModelScope = ModelScope::new();
        let (x, m, c): (SourceNode<u32>, DerivedNode<u32>, DerivedNode<u32>) = scope.batch(|| {
            let x: SourceNode<u32> = node!(1);
            let m: DerivedNode<u32> = node!(|x| *x.get() + 1);
            // Reading the ghost fails, so the survivor's rerun counts it as 0.
            let c: DerivedNode<u32> = node!(|x, m| *x.get() + m.try_get().map_or(0, |v: std::sync::Arc<u32>| *v));
            (x, m, c)
        });
        scope.flush();
        let doomed: DerivedNode<u32> = m.clone();
        let disposer: loom::thread::JoinHandle<()> = loom::thread::spawn(move || lithia::dispose(&doomed));
        scope.batch(|| x.set(2));
        disposer.join().unwrap();
        scope.flush();
        assert!(matches!(lithia::status(&m), Status::Ghost));
        assert_eq!(*c.get(), 2);
        assert!(model::quiet(&x));
    });
}

/// A last-handle drop on one thread races a top-level creation on the same node on another;
/// both nodes are freed whichever lands first (spec §10.1, §11.4 step 1).
#[test]
fn a_last_handle_drop_races_a_creation() {
    check(|| {
        let scope: ModelScope = ModelScope::new();
        let (n, seen): (SourceNode<u32>, SourceNode<u32>) = scope.batch(|| (node!(1), node!(0)));
        let (other, seen_there): (SourceNode<u32>, SourceNode<u32>) = (n.clone(), seen.clone());
        let creator: loom::thread::JoinHandle<ModelScope> = loom::thread::spawn(move || {
            let there: ModelScope = ModelScope::new();
            let made: DerivedNode<()> = there.batch(|| node!(|other| seen_there.set(*other.get())));
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
        let p: SourceNode<u32> = scope.batch(|| node!(1));
        let parent: SourceNode<u32> = p.clone();
        let creator: loom::thread::JoinHandle<(ModelScope, DerivedNode<u32>)> = loom::thread::spawn(move || {
            let there: ModelScope = ModelScope::new();
            let orphan: DerivedNode<u32> = there.batch(|| node!(|parent| *parent.get()));
            (there, orphan)
        });
        lithia::dispose(&p);
        let (there, orphan): (ModelScope, DerivedNode<u32>) = creator.join().unwrap();
        scope.flush();
        there.flush();
        assert!(matches!(lithia::status(&orphan), Status::Ghost));
    });
}

/// A non-resilient node created, and run on the worker, while one of its parents is disposed ends
/// failed: it reruns as a surviving child, or its first run already sees the ghost.
#[test]
fn a_creation_races_the_disposal_of_one_parent() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (p, k): (SourceNode<u32>, SourceNode<u32>) = scope.batch(|| (node!(1), node!(2)));
            let (parent, live): (SourceNode<u32>, SourceNode<u32>) = (p.clone(), k.clone());
            let creator: loom::thread::JoinHandle<(ModelScope, DerivedNode<u32>)> = loom::thread::spawn(move || {
                let there: ModelScope = ModelScope::new();
                let strict: DerivedNode<u32> = there.batch(|| node!(|parent, live| *live.get(), resilience = NONE));
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
                let t: SourceNode<u32> = node!(1);
                node!(|t| {
                    let _ = t.get();
                });
                node!(async move {
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

/// A moved-out handle drops on the main thread while its block's lease ends on the worker, after
/// support let go of `e`: nothing stays held either way (found by TLC, `docs/tla`).
#[test]
fn a_moved_out_handle_drops_while_its_lease_ends_and_support_has_let_go() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let slot: Arc<(loom::sync::Mutex<Option<SourceNode<u32>>>, loom::sync::Condvar)> =
                Arc::new((loom::sync::Mutex::new(None), loom::sync::Condvar::new()));
            let out: Arc<(loom::sync::Mutex<Option<SourceNode<u32>>>, loom::sync::Condvar)> = slot.clone();
            let (y, z): (SourceNode<u32>, SourceNode<u32>) = scope.batch(|| {
                let t: SourceNode<u32> = node!(1);
                let y: SourceNode<u32> = node!(1);
                let z: SourceNode<u32> = node!(1);
                let e: DerivedNode<u32> = node!(|t| *t.get());
                node!(|e, y| {
                    let _ = (e.get(), y.get());
                });
                let block: SourceNode<()> = node!(async move {
                    *out.0.lock().unwrap() = Some(t);
                    out.1.notify_one();
                });
                // Keeps the block alive without anchoring it, so support lets go of `e`.
                node!(|z| {
                    let _ = (z.get(), block.get());
                });
                (y, z)
            });
            let moved: SourceNode<u32> = {
                let mut guard: loom::sync::MutexGuard<'_, Option<SourceNode<u32>>> = slot.0.lock().unwrap();
                loop {
                    if let Some(handle) = guard.take() {
                        break handle;
                    }
                    guard = slot.1.wait(guard).unwrap();
                }
            };
            drop(moved);
            scope.flush();
            drop(y); // `e` and its child go; `z`, the block and its keeper stay
            scope.flush();
            assert_eq!(scope.live(), 3);
            drop(z);
            scope.flush();
            assert_eq!(scope.live(), 0);
        });
    });
}

/// A parent's and its child's last handles drop at once: both are examined, and both freed.
#[test]
fn a_parent_and_its_child_drop_their_last_handles_at_once() {
    check(|| {
        let scope: ModelScope = ModelScope::new();
        let (p, c): (SourceNode<u32>, DerivedNode<u32>) = scope.batch(|| {
            let p: SourceNode<u32> = node!(1);
            let c: DerivedNode<u32> = node!(|p| *p.get());
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
                let s: SourceNode<u32> = node!(1);
                let trigger: SourceNode<u32> = node!(0);
                let m: DerivedNode<u32> = node!(|s| *s.get());
                node!(|m| {
                    let _ = m.get();
                });
                node!(|trigger| {
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
            let s: SourceNode<u32> = node!(1);
            let u: SourceNode<u32> = node!(2);
            let m: DerivedNode<u32> = node!(|s| *s.get());
            node!(|m, u| {
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
                let x: SourceNode<u32> = node!(0);
                let t: SourceNode<u32> = node!(1);
                node!(|x| {
                    let _ = x.get();
                });
                node!(|t| {
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

/// A sweep copies a non-resilient child's parent list while the worker decides whether its
/// parents fail it, which takes the two locks the other way round; neither waits on the other.
#[test]
fn a_sweep_alongside_a_resilience_decision() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (n, k): (SourceNode<i32>, SourceNode<i32>) = scope.batch(|| {
                let n: SourceNode<i32> = node!(1);
                let k: SourceNode<i32> = node!(1);
                let inv: DerivedNode<i32> = node!(|n| 100 / *n.get());
                node!(|inv, k| *inv.get() + *k.get(), resilience = NONE);
                drop(inv);
                (n, k)
            });
            scope.flush();
            scope.batch(|| n.set(0));
            drop(k); // a sweep from `k` over the non-resilient child
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
            let a: SourceNode<u32> = node!(1);
            let b: SourceNode<u32> = node!(0);
            let k: SourceNode<u32> = node!(0);
            let seen: SourceNode<u32> = node!(0);
            node!(|b, k| {
                let _ = k.get();
                seen.set(*b.get());
            });
            (a, b, k, seen)
        });
        scope.flush();
        let other: loom::thread::JoinHandle<()> = loom::thread::spawn(move || drop(k));
        let from_a: SourceNode<u32> = a.clone();
        scope.batch(move || {
            node!(move |from_a| b.set(*from_a.get() * 2));
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
            let rates: SourceNode<u32> = node!(2);
            let input: SourceNode<u32> = node!(3);
            let price: DerivedNode<u32> = node!(|input, rates| *input.get() * *rates.get()).scoped();
            node!(|price, rates| {
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
