// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Runs parked on a future that never wakes them: the protocol must (spec §11.4).

use super::*;

/// Pending until polled after a wake from elsewhere: only lithia's wakes reach a task parked on it.
struct Parked;

impl std::future::Future for Parked {
    type Output = ();

    fn poll(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        std::task::Poll::Pending
    }
}

/// A write stops an async source's block parked on a future that never wakes it: the write's
/// wake lets the block see it was stopped and end (spec §7.3 item 5).
#[test]
fn a_write_wakes_the_parked_block_it_stops() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (fed, copy): (SourceNode<u32>, DerivedNode<u32>) = scope.batch(|| {
                let fed: SourceNode<u32> = effect!(async {
                    Parked.await;
                    1
                });
                let copy: DerivedNode<u32> = effect!(|fed| *fed.get());
                (fed, copy)
            });
            model::run_one();
            scope.batch(|| fed.set(2));
            scope.settle(&fed);
            assert_eq!((*fed.get(), *copy.get()), (2, 2));
            assert!(model::quiet(&fed));
        });
    });
}

/// Disposal ends a run parked on a future that never wakes it: retiring the node wakes the run,
/// which finds it gone and drops its future (spec §10.4).
#[test]
fn disposal_wakes_a_parked_run() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (a, waiting): (SourceNode<u32>, DerivedNode<u32>) = scope.batch(|| {
                let a: SourceNode<u32> = effect!(1);
                let waiting: DerivedNode<u32> = effect!(async |a| {
                    let value: u32 = *a.get();
                    Parked.await;
                    value
                });
                (a, waiting)
            });
            model::run_one();
            lithia::dispose(&waiting);
            scope.settle(&a);
            drop((a, waiting));
            scope.flush();
            assert_eq!(scope.live(), 0);
        });
    });
}

/// A node freed with its run parked on a future that never wakes it: the node's drop wakes the
/// run, which finds the node gone and drops its future, captures included (spec §10.1).
#[test]
fn freeing_a_node_wakes_its_parked_run() {
    check(|| {
        with_worker(|| {
            let scope: ModelScope = ModelScope::new();
            let (a, waiting): (SourceNode<u32>, DerivedNode<u32>) = scope.batch(|| {
                let a: SourceNode<u32> = effect!(1);
                let waiting: DerivedNode<u32> = effect!(async |a| {
                    let value: u32 = *a.get();
                    Parked.await;
                    value
                });
                (a, waiting)
            });
            model::run_one();
            drop(waiting);
            drop(a);
            scope.flush();
            assert_eq!(scope.live(), 0);
        });
    });
}
