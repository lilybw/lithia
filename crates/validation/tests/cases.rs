// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Validation cases A–AG from spec §13, verbatim, with Case J as rewritten for v0.3.0; Case AH
//! lives in `compile.rs`, and the cases v0.3.0 adds in `support.rs`.
//! Every case runs under the workspace's test timeout (`.cargo/config.toml`).

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lithia::{Accessor, Changes, DerivedNode, Setter, SourceNode, Status, effect};
use tokio::sync::oneshot;

// Case A: nested creation and a write-back stop.
// Must compile, add exactly one element, run the expansion once and leave callcount at 1.
#[lithia::test]
async fn case_a_nested_creation() {
    let array: SourceNode<Vec<Accessor<i32>>> = effect!(Vec::new());
    let callcount: SourceNode<i32> = effect!(0);

    let _expansion: DerivedNode<()> = effect!(|array| {
        callcount.update(|n: &i32| n + 1);
        let c: SourceNode<i32> = effect!(42);
        println!("expansion memo run {}", callcount.get()); // prints 1: bodies see their own writes
        array.update(|old: &Vec<Accessor<i32>>| {
            // applied; `_expansion` itself is cut (7.4)
            let mut v: Vec<Accessor<i32>> = old.clone();
            v.push(c.accessor());
            v
        });
    });

    array.set(Vec::new());
    lithia::flush().await;

    assert_eq!(*callcount.get(), 1);
    assert_eq!(array.get().len(), 1);
}

// Case B: diamond.
#[lithia::test]
async fn case_b_diamond() {
    let a: SourceNode<i32> = effect!(1);
    let b: DerivedNode<i32> = effect!(|a| *a.get() + 1);
    let c: DerivedNode<i32> = effect!(|a| *a.get() * 10);
    let runs: SourceNode<u32> = effect!(0);
    let d: DerivedNode<i32> = effect!(|b, c| {
        runs.update(|n: &u32| n + 1);
        *b.get() + *c.get()
    });
    lithia::flush().await;
    assert_eq!((*d.get(), *runs.get()), (12, 1));

    a.set(2);
    lithia::flush().await;
    assert_eq!((*d.get(), *runs.get()), (23, 2)); // one run per change, never 3 + 10 or 2 + 20
}

// Case C: clamping cycle.
#[lithia::test]
async fn case_c_clamp_cycle() {
    let x: SourceNode<i32> = effect!(0);
    let runs: SourceNode<u32> = effect!(0);
    let _clamp: DerivedNode<()> = effect!(|x| {
        runs.update(|n: &u32| n + 1);
        if *x.get() > 10 {
            x.set(10);
        }
    });
    x.set(15);
    lithia::flush().await;
    assert_eq!((*x.get(), *runs.get()), (10, 1)); // the write-back stops at x
}

// Case D: optimistic async.
#[lithia::test]
async fn case_d_optimistic_async() {
    let (tx, rx): (oneshot::Sender<()>, oneshot::Receiver<()>) = oneshot::channel();
    let gate: Arc<Mutex<Option<oneshot::Receiver<()>>>> = Arc::new(Mutex::new(Some(rx)));
    let id: SourceNode<u32> = effect!(7);
    let name: DerivedNode<String> = effect!(async |id| {
        let rx: Option<oneshot::Receiver<()>> = gate.lock().unwrap().take();
        if let Some(rx) = rx {
            rx.await.ok();
        }
        format!("user-{}", id.get())
    });
    let seen: SourceNode<Vec<String>> = effect!(Vec::new());
    let _view: DerivedNode<()> = effect!(|name| {
        let current: String = (*name.get()).clone();
        seen.update(|old: &Vec<String>| {
            let mut v: Vec<String> = old.clone();
            v.push(current);
            v
        });
    });
    lithia::flush().await; // `name` waits on the gate; `_view` ran on the prior value
    tx.send(()).unwrap(); // only now let the fetch finish
    lithia::test::settled(&name).await; // the async result, and the rerun it caused
    assert_eq!(*seen.get(), vec![String::new(), "user-7".to_string()]); // prior value, then the async result
}

// Case E: panic, recovery and fragility.
#[lithia::test]
async fn case_e_panic_and_fragility() {
    let n: SourceNode<i32> = effect!(1);
    let inv: DerivedNode<i32> = effect!(|n| 100 / *n.get());
    let strict: DerivedNode<i32> = effect!(|inv| *inv.get() + 1, fragility = FRAGILE);
    let lenient: DerivedNode<i32> = effect!(|inv| *inv.get() + 2);
    lithia::flush().await;

    n.set(0); // integer division by zero panics
    lithia::flush().await;
    assert!(matches!(lithia::status(&inv), Status::Failed(_)));
    assert_eq!(*inv.get(), 100); // last successful value kept
    assert!(matches!(lithia::status(&strict), Status::Failed(_)));
    assert!(matches!(lithia::status(&lenient), Status::Ready));

    n.set(4);
    lithia::flush().await;
    assert_eq!((*inv.get(), *strict.get()), (25, 26));
    assert!(matches!(lithia::status(&strict), Status::Ready));
}

// Case F: ghosts.
#[lithia::test]
async fn case_f_ghosts_and_views() {
    let s: SourceNode<i32> = effect!(1);
    let reader: Accessor<i32> = s.accessor();
    let writer: Setter<i32> = s.setter();
    let t: SourceNode<i32> = effect!(5, ghost = ON_GHOST_EMIT_LATEST);
    let child: DerivedNode<i32> = effect!(|s| *s.get() * 2);
    // effect!(|writer| 0) would not compile: `Setter` does not implement `Readable`
    lithia::flush().await;

    lithia::dispose(&s);
    lithia::dispose(&s); // second disposal does nothing
    assert!(reader.try_get().is_err());
    assert!(writer.try_set(2).is_err());
    assert!(child.try_get().is_err()); // its only parent was freed, so the cascade took it too

    lithia::dispose(&t);
    assert_eq!(*t.get(), 5); // latest value stays readable
    assert!(t.try_set(6).is_err()); // writes fail under every policy
}

// Case G: async source node.
#[lithia::test]
async fn case_g_async_source() {
    let user: SourceNode<String> = effect!(
        async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            "loaded".to_string()
        },
        init = "guest".to_string()
    );
    assert!(matches!(lithia::status(&user), Status::Pending));
    assert_eq!(user.get().as_str(), "guest"); // initial value while pending
    lithia::test::settled(&user).await;
    assert_eq!(user.get().as_str(), "loaded");

    let slow: SourceNode<u32> = effect!(async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        1
    });
    slow.set(2); // supersedes the pending block, which is aborted
    lithia::test::settled(&slow).await;
    assert_eq!(*slow.get(), 2);
}

// Case H: sibling of a write cycle.
#[lithia::test]
async fn case_h_cycle_sibling() {
    let x: SourceNode<i32> = effect!(0);
    let _clamp: DerivedNode<()> = effect!(|x| if *x.get() > 10 {
        x.set(10);
    });
    let shown: DerivedNode<i32> = effect!(|x| *x.get());
    x.set(15);
    lithia::flush().await;
    assert_eq!(*shown.get(), 10); // the sibling's final value is the clamped one; it may briefly see 15 first
}

// Case I: returning a derived node.
fn counter() -> DerivedNode<i32> {
    let c: SourceNode<i32> = effect!(0);
    effect!(|c| *c.get() + 1)
}

#[lithia::test]
async fn case_i_returned_memo() {
    let m: DerivedNode<i32> = counter();
    lithia::flush().await;
    assert_eq!(*m.get(), 1);
    assert!(matches!(lithia::status(&m), Status::Ready)); // `c` lives as long as `m` holds it
}

// Case J: an effect lives while its parent can change.
#[lithia::test]
async fn case_j_an_effect_lives_while_its_parent_can_change() {
    let count: SourceNode<i32> = effect!(0);
    let runs: SourceNode<u32> = effect!(0);
    effect!(|count| runs.update(|n: &u32| n + 1)); // lives while `count` is held
    let base: usize = lithia::test::live_nodes().await;
    {
        let local: SourceNode<i32> = effect!(0);
        effect!(|local| runs.update(|n: &u32| n + 1)); // writing `runs` does not keep it alive
        lithia::flush().await;
        assert_eq!(lithia::test::live_nodes().await, base + 2);
    } // `local` drops here, and its effect is freed with it
    assert_eq!(lithia::test::live_nodes().await, base);
    count.set(1);
    lithia::flush().await;
    assert_eq!(*runs.get(), 3);
}

#[lithia::test]
async fn case_k_generator() {
    let count_to: SourceNode<u32> = effect!(3);
    let tick: DerivedNode<u32> = effect!(|count_to| {
        for i in 1..=*count_to.get() {
            yield i;
        }
    });
    let seen: SourceNode<Vec<u32>> = effect!(Vec::new());
    let _log: DerivedNode<()> = effect!(|tick| {
        let t: u32 = *tick.get();
        seen.update(|old: &Vec<u32>| {
            let mut v: Vec<u32> = old.clone();
            v.push(t);
            v
        });
    });
    lithia::test::settled(&tick).await; // waits for the generator to end
    assert_eq!(*tick.get(), 3);
    assert_eq!(seen.get().last(), Some(&3)); // a slow child may skip items, but the last one always lands

    count_to.set(2);
    lithia::test::settled(&tick).await;
    assert_eq!(*tick.get(), 2); // the rerun replaced the generator
}

// Case L: yielding before an await.
#[lithia::test]
async fn case_l_yield() {
    let (tx, rx): (oneshot::Sender<()>, oneshot::Receiver<()>) = oneshot::channel();
    let gate: Arc<Mutex<Option<oneshot::Receiver<()>>>> = Arc::new(Mutex::new(Some(rx)));
    let url: SourceNode<String> = effect!("a".to_string());
    let status: DerivedNode<String> = effect!(async |url| {
        yield "loading".to_string();
        let rx: Option<oneshot::Receiver<()>> = gate.lock().unwrap().take();
        if let Some(rx) = rx {
            rx.await.ok();
        }
        yield format!("done: {}", url.get());
    });
    lithia::flush().await; // runs until the body waits on the gate
    assert_eq!(status.get().as_str(), "loading"); // the first yield published
    assert!(matches!(lithia::status(&status), Status::Ready)); // Ready, though the run continues
    tx.send(()).unwrap();
    lithia::test::settled(&status).await;
    assert_eq!(status.get().as_str(), "done: a");
}

// Case M: clamping through a memo.
#[lithia::test]
async fn case_m_clamp_through_memo() {
    let x: SourceNode<i32> = effect!(0);
    let doubled: DerivedNode<i32> = effect!(|x| *x.get() * 2);
    let _clamp: DerivedNode<()> = effect!(|doubled| if *doubled.get() > 20 {
        x.set(10);
    });
    x.set(15);
    lithia::flush().await;
    assert_eq!((*x.get(), *doubled.get()), (10, 20)); // the pure node reruns; only the writer is cut
}

// Case N: node created mid-propagation.
#[lithia::test(flavor = "multi_thread", worker_threads = 2)]
async fn case_n_created_mid_propagation() {
    let (started_tx, started_rx): (mpsc::Sender<()>, mpsc::Receiver<()>) = mpsc::channel();
    let (go_tx, go_rx): (mpsc::Sender<()>, mpsc::Receiver<()>) = mpsc::channel();
    let started_rx: Arc<Mutex<mpsc::Receiver<()>>> = Arc::new(Mutex::new(started_rx));
    let go_rx: Arc<Mutex<mpsc::Receiver<()>>> = Arc::new(Mutex::new(go_rx));
    let base: usize = lithia::test::live_nodes().await;
    let a: SourceNode<i32> = effect!(1);
    let slow: DerivedNode<i32> = effect!(|a| {
        started_tx.send(()).unwrap();
        go_rx.lock().unwrap().recv().unwrap(); // still running while `m` is created
        *a.get()
    });
    let holder: SourceNode<Option<DerivedNode<i32>>> = effect!(None);
    let _maker: DerivedNode<()> = effect!(|a| {
        started_rx.lock().unwrap().recv().unwrap();
        holder.set(Some(effect!(|slow| *slow.get() + 1)));
        go_tx.send(()).unwrap();
    });
    lithia::flush().await;
    let m: DerivedNode<i32> = (*holder.get()).clone().expect("set by _maker");
    assert_eq!(*m.get(), 2); // `m`'s entry waited on `slow`'s in-flight run, so `m` ran once, after it
    drop((a, slow, holder, _maker, m));
    assert_eq!(lithia::test::live_nodes().await, base); // v0.3.0: nothing holds itself
}

// Case O: a run that yields nothing.
#[lithia::test]
async fn case_o_yield_nothing() {
    let on: SourceNode<bool> = effect!(true);
    let last: DerivedNode<u32> = effect!(
        |on| {
            if *on.get() {
                yield 7;
            }
        },
        init = 1
    );
    lithia::test::settled(&last).await;
    assert_eq!(*last.get(), 7);

    on.set(false); // this run yields nothing
    lithia::test::settled(&last).await;
    assert_eq!(*last.get(), 7); // the latest item stays
    assert!(matches!(lithia::status(&last), Status::Ready));
}

// Case P: unconditional write-back.
#[lithia::test]
async fn case_p_unconditional_writeback() {
    let x: SourceNode<i32> = effect!(0);
    let doubled: DerivedNode<i32> = effect!(|x| *x.get() * 2);
    let _back: DerivedNode<()> = effect!(|doubled| x.set(*doubled.get() / 2 + 1));
    x.set(5);
    lithia::flush().await; // returns: the writer check applies at every step
    assert_eq!((*x.get(), *doubled.get()), (6, 12));
}

// Case Q: a cut scoped to its item.
#[lithia::test]
async fn case_q_cut_scoped_to_its_item() {
    let tick: SourceNode<u32> = effect!(async {
        for i in 1..=2u32 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            yield i;
        }
    });
    let prefs: SourceNode<u32> = effect!(0);
    let audit: SourceNode<u32> = effect!(0);
    let login: DerivedNode<u32> = effect!(|tick, prefs| {
        if *tick.get() == 1 {
            audit.set(1);
        } // writes on item 1 only
        *prefs.get()
    });
    let _sync: DerivedNode<()> = effect!(|login, tick| if *tick.get() == 2 {
        prefs.set(42);
    });
    lithia::test::settled(&tick).await;
    assert_eq!(*login.get(), 42); // item 2's epoch inherits tick's empty writer set, not item 1's
}

// Case R: an equal parent stops propagation.
#[lithia::test]
async fn case_r_equal_parent_stops_propagation() {
    let a: SourceNode<i32> = effect!(1);
    let parity: DerivedNode<i32> = effect!(|a| *a.get() % 2, equals = lithia::eq);
    let runs: SourceNode<u32> = effect!(0);
    let _e: DerivedNode<()> = effect!(|parity| runs.update(|n: &u32| n + 1));
    lithia::flush().await;
    a.set(3); // parity stays 1
    lithia::flush().await;
    assert_eq!(*runs.get(), 1); // `_e` was released as unchanged
}

// Case S: a loop through node creation is cut.
#[lithia::test]
async fn case_s_loop_through_creation() {
    let n: SourceNode<u32> = effect!(0);
    let _holder: DerivedNode<Option<DerivedNode<()>>> = effect!(|n| { Some(effect!(|n| n.update(|v: &u32| v + 1))) });
    lithia::flush().await; // returns: the child the rerun makes writes under the first child's lineage
    assert_eq!(*n.get(), 1);
}

// Case T: a refused writer releases its children.
#[lithia::test]
async fn case_t_cut_writer_releases_children() {
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
    lithia::flush().await; // returns only because the refused `clamp` settles unchanged
    assert!(!*clamp.get()); // then it reran once the epoch settled, on doubled = 20 (7.4)
    y.set(1);
    lithia::flush().await;
    assert_eq!(*shown.get(), 1); // and `shown` still follows `y`
}

// Case U: a loader feeds its creator.
#[lithia::test]
async fn case_u_loader_feeds_its_creator() {
    let base: usize = lithia::test::live_nodes().await;
    let id: SourceNode<u32> = effect!(1);
    let cache: SourceNode<Option<String>> = effect!(None, equals = lithia::eq);
    let view: DerivedNode<(String, Option<DerivedNode<()>>)> = effect!(|id, cache| {
        let current: Option<String> = (*cache.get()).clone();
        let loader: Option<DerivedNode<()>> = match current.as_ref() {
            Some(_) => None,
            None => Some(effect!(async |id| {
                tokio::time::sleep(Duration::from_millis(10)).await;
                cache.set(Some(format!("user-{}", id.get())));
            })),
        };
        (current.unwrap_or_else(|| "loading".to_string()), loader)
    });
    lithia::test::settled(&view).await;
    assert_eq!(view.get().0, "user-1"); // the loader's write reruns `view`: a creator is not in its child's writer set
    drop((id, cache, view));
    assert_eq!(lithia::test::live_nodes().await, base); // v0.3.0: nothing holds itself
}

// Case V: reflow reruns children on an equal value.
#[lithia::test]
async fn case_v_reflow() {
    let a: SourceNode<i32> = effect!(1, equals = lithia::eq);
    let runs: SourceNode<u32> = effect!(0);
    let _child: DerivedNode<()> = effect!(|a| runs.update(|n: &u32| n + 1));
    lithia::flush().await;
    a.set(1); // equal, so nothing propagates
    lithia::flush().await;
    assert_eq!(*runs.get(), 1);
    lithia::reflow(&a); // raises the version: the child reruns on the same value
    lithia::flush().await;
    assert_eq!(*runs.get(), 2);
}

// Case W: reflow from a node's own body.
#[lithia::test]
async fn case_w_reflow_from_its_own_body() {
    let base: usize = lithia::test::live_nodes().await;
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
    let _child: DerivedNode<()> = effect!(|node| child_runs.update(|n: &u32| n + 1));
    lithia::flush().await;
    me.set(Some(node.clone())); // a value cycle, broken at the end (10.1)
    tick.set(0); // the same value: only the reflow can reach the child
    lithia::flush().await;
    assert_eq!((*body_runs.get(), *child_runs.get()), (2, 2)); // the node did not rerun itself; its child did
    me.set(None);
    lithia::flush().await;
    drop((me, tick, body_runs, child_runs, node, _child));
    assert_eq!(lithia::test::live_nodes().await, base); // v0.3.0: nothing holds itself
}

// Case X: defer feeds a value back on purpose.
#[lithia::test]
async fn case_x_defer_feeds_back_on_purpose() {
    let x: SourceNode<u32> = effect!(0);
    let runs: SourceNode<u32> = effect!(0);
    let _bump: DerivedNode<()> = effect!(|x| {
        runs.update(|n: &u32| n + 1);
        if *x.get() == 0 {
            let later: SourceNode<u32> = x.clone();
            lithia::defer(move || later.set(1)); // runs after this epoch settles, in a fresh one (6.3)
        }
    });
    let mut seen: Changes<u32> = runs.changes();
    while let Some(n) = seen.next().await {
        if *n == 2 {
            break;
        } // `_bump` reran: a deferred write is not cut; a cut would time out
    }
    assert_eq!(*x.get(), 1);
}

// Case Y: changes() yields the current value first and ends at disposal.
#[lithia::test]
async fn case_y_changes() {
    let n: SourceNode<u32> = effect!(1);
    let mut stream: Changes<u32> = n.changes();
    assert_eq!(*stream.next().await.unwrap(), 1); // the current value first
    n.set(2);
    assert_eq!(*stream.next().await.unwrap(), 2);
    lithia::dispose(&n);
    assert!(stream.next().await.is_none()); // the stream ends with the node (10.5)
}

// Case Z: modify publishes a changed copy.
#[lithia::test]
async fn case_z_modify() {
    let history: SourceNode<Vec<u32>> = effect!(Vec::new(), equals = lithia::eq);
    let runs: SourceNode<u32> = effect!(0);
    let _watch: DerivedNode<()> = effect!(|history| runs.update(|n: &u32| n + 1));
    lithia::flush().await;
    let before: Arc<Vec<u32>> = history.get();
    history.modify(|v: &mut Vec<u32>| v.push(1));
    lithia::flush().await;
    assert_eq!(*history.get(), vec![1]);
    assert!(before.is_empty()); // a reader's old value never changes under it (6.1)
    assert_eq!(*runs.get(), 2);
    history.modify(|_v: &mut Vec<u32>| {}); // an equal result propagates nothing (6.4)
    lithia::flush().await;
    assert_eq!(*runs.get(), 2);
}

// Case AA: spawn_blocking from a body.
#[lithia::test]
async fn case_aa_spawn_blocking_from_a_body() {
    let input: SourceNode<u32> = effect!(3);
    let result: SourceNode<u32> = effect!(0);
    let runs: SourceNode<u32> = effect!(0);
    let work: DerivedNode<()> = effect!(async |input, result| {
        runs.update(|n: &u32| n + 1);
        let (n, out): (u32, SourceNode<u32>) = (*input.get(), result.clone());
        lithia::spawn_blocking(move || out.set(n * n)).await.unwrap(); // one of this run's deliveries (7.1)
    });
    lithia::test::settled(&work).await;
    assert_eq!((*result.get(), *runs.get()), (9, 1)); // the write-back is attributed, so `work` is cut
}

// Case AB: SEMI_FRAGILE fails only when every parent has failed.
#[lithia::test]
async fn case_ab_semi_fragile() {
    let a: SourceNode<i32> = effect!(1);
    let b: SourceNode<i32> = effect!(1);
    let inv_a: DerivedNode<i32> = effect!(|a| 100 / *a.get());
    let inv_b: DerivedNode<i32> = effect!(|b| 100 / *b.get());
    let sum: DerivedNode<i32> = effect!(|inv_a, inv_b| *inv_a.get() + *inv_b.get(), fragility = SEMI_FRAGILE);
    lithia::flush().await;
    a.set(0); // one parent fails
    lithia::flush().await;
    assert!(matches!(lithia::status(&sum), Status::Ready));
    b.set(0); // now every parent has
    lithia::flush().await;
    assert!(matches!(lithia::status(&sum), Status::Failed(_)));
}

// Case AC: a ghost with a surviving child.
#[lithia::test]
async fn case_ac_ghost_with_a_surviving_child() {
    let kept: SourceNode<i32> = effect!(1);
    let doomed: SourceNode<i32> = effect!(10, ghost = ON_GHOST_SET_AS_PENDING);
    let both: DerivedNode<i32> = effect!(|kept, doomed| *kept.get() + *doomed.get());
    lithia::flush().await;
    lithia::dispose(&doomed); // `both` survives through `kept` (10.4)
    lithia::flush().await;
    assert!(matches!(lithia::status(&doomed), Status::Ghost));
    kept.set(2);
    lithia::flush().await;
    assert_eq!(*both.get(), 12); // the ghost still reads its last value
    assert!(matches!(lithia::status(&both), Status::Ready)); // a pending parent fails nobody
}

// Case AD: ALWAYS_PROPAGATE.
#[lithia::test]
async fn case_ad_always_propagate() {
    let a: SourceNode<i32> = effect!(1, equals = lithia::eq, flags = ALWAYS_PROPAGATE);
    let runs: SourceNode<u32> = effect!(0);
    let _child: DerivedNode<()> = effect!(|a| runs.update(|n: &u32| n + 1));
    lithia::flush().await;
    let before: Arc<i32> = a.get();
    a.set(1); // equal: the old value is kept...
    lithia::flush().await;
    assert!(Arc::ptr_eq(&before, &a.get()));
    assert_eq!(*runs.get(), 2); // ...but the child reruns anyway (6.4)
}

// Case AE: a panicking body still propagates its writes.
#[lithia::test]
async fn case_ae_panic_still_propagates_writes() {
    let go: SourceNode<bool> = effect!(false);
    let log: SourceNode<Vec<&'static str>> = effect!(Vec::new());
    let faulty: DerivedNode<()> = effect!(|go| {
        if *go.get() {
            log.update(|v: &Vec<&'static str>| {
                let mut w: Vec<&'static str> = v.clone();
                w.push("before the panic");
                w
            });
            panic!("boom");
        }
    });
    let seen: SourceNode<usize> = effect!(0);
    let _reader: DerivedNode<()> = effect!(|log| seen.set(log.get().len()));
    lithia::flush().await;
    go.set(true);
    lithia::flush().await;
    assert_eq!(*seen.get(), 1); // the write before the panic propagated (6.2)
    assert!(matches!(lithia::status(&faulty), Status::Failed(_)));
}

// Case AF: live_nodes catches a value cycle.
#[lithia::test]
async fn case_af_live_nodes_catch_a_value_cycle() {
    let base: usize = lithia::test::live_nodes().await;
    {
        let _tmp: SourceNode<i32> = effect!(0);
    }
    assert_eq!(lithia::test::live_nodes().await, base); // freed when its last handle dropped
    {
        let holder: SourceNode<Option<DerivedNode<bool>>> = effect!(None);
        let child: DerivedNode<bool> = effect!(|holder| holder.get().is_some());
        holder.set(Some(child.clone())); // the holder's value holds its own subscriber (10.1)
        lithia::flush().await;
    }
    assert_eq!(lithia::test::live_nodes().await, base + 2); // the cycle leaked, and the count shows it
}

// Case AG: a graph spanning two runtimes.
#[test]
fn case_ag_graph_across_runtimes() {
    let rt_a: tokio::runtime::Runtime = tokio::runtime::Runtime::new().unwrap();
    let rt_b: tokio::runtime::Runtime = tokio::runtime::Runtime::new().unwrap();
    let src: SourceNode<i32> = rt_a.block_on(async { effect!(1) });
    let doubled: DerivedNode<i32> = rt_b.block_on(async { effect!(|src| *src.get() * 2) });
    rt_a.block_on(async {
        src.set(5);
        lithia::flush().await; // waits for `doubled`, whose runs stay on rt_b (11.3)
    });
    assert_eq!(*doubled.get(), 10);
}
