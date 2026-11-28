// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Heap allocations per operation, counted by a global allocator; steadier than wall time on a
//! shared host. Run with `cargo bench -p lithia-bench --bench allocs`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

use lithia::{DerivedNode, SourceNode, node};

/// Counts every allocation and reallocation, then defers to the system allocator.
struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

// SAFETY: every method forwards to `System` with the caller's arguments unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: forwarded from the caller, who upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: as for `alloc`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System` through this allocator, with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: as for `dealloc`; the caller upholds `realloc`'s size contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn count() -> u64 {
    ALLOCATIONS.load(Ordering::Relaxed)
}

fn per(allocations: u64, operations: u64) -> String {
    format!("{:.1}", allocations as f64 / operations.max(1) as f64)
}

/// Runs a future as a lithia task on a one-thread runtime, so counts do not depend on stealing.
fn run<F>(future: F) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let rt: tokio::runtime::Runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (tx, rx): (tokio::sync::oneshot::Sender<F::Output>, tokio::sync::oneshot::Receiver<F::Output>) =
            tokio::sync::oneshot::channel();
        lithia::spawn(async move {
            let _ = tx.send(future.await);
        });
        rx.await.expect("the benchmark task panicked")
    })
}

/// Allocations per run along a chain of synchronous nodes.
fn chain() -> String {
    const LENGTH: u64 = 1_000;
    const ROUNDS: u64 = 20;
    let allocations: u64 = run(async {
        let source: SourceNode<u64> = node!(0);
        let mut tail: DerivedNode<u64> = node!(|source| *source.get() + 1);
        let mut chain: Vec<DerivedNode<u64>> = Vec::new();
        for _ in 1..LENGTH {
            let previous: DerivedNode<u64> = tail.clone();
            chain.push(previous.clone());
            tail = node!(|previous| *previous.get() + 1);
        }
        lithia::flush().await;
        let before: u64 = count();
        for round in 1..=ROUNDS {
            source.set(round);
            lithia::flush().await;
        }
        count() - before
    });
    format!("chain: {} per run", per(allocations, LENGTH * ROUNDS))
}

/// Allocations per child run on a wide fan-out.
fn fan_out() -> String {
    const WIDTH: u64 = 1_000;
    const ROUNDS: u64 = 20;
    let allocations: u64 = run(async {
        let source: SourceNode<u64> = node!(0);
        let children: Vec<DerivedNode<u64>> = (0..WIDTH).map(|_| node!(|source| *source.get())).collect();
        lithia::flush().await;
        let before: u64 = count();
        for round in 1..=ROUNDS {
            source.set(round);
            lithia::flush().await;
        }
        drop(children);
        count() - before
    });
    format!("fan-out: {} per child run", per(allocations, WIDTH * ROUNDS))
}

/// Allocations per detached write, with one child that reruns.
fn detached_write() -> String {
    const WRITES: u64 = 2_000;
    let rt: tokio::runtime::Runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    // A task lithia did not start: each write is its own batch in the task's scope.
    let allocations: u64 = rt
        .block_on(rt.spawn(async {
            let source: SourceNode<u64> = node!(0);
            let child: DerivedNode<u64> = node!(|source| *source.get());
            lithia::flush().await;
            let before: u64 = count();
            for round in 1..=WRITES {
                source.set(round);
                tokio::task::yield_now().await;
            }
            lithia::flush().await;
            std::hint::black_box(*child.get());
            count() - before
        }))
        .unwrap();
    format!("detached write with one child: {} per write", per(allocations, WRITES))
}

/// Allocations per creation of a top-level effect, first run included.
fn creation() -> String {
    const COUNT: u64 = 2_000;
    let allocations: u64 = run(async {
        let a: SourceNode<u64> = node!(0);
        lithia::flush().await;
        let before: u64 = count();
        let kept: Vec<DerivedNode<u64>> = (0..COUNT).map(|_| node!(|a| *a.get())).collect();
        lithia::flush().await;
        let allocations: u64 = count() - before;
        drop(kept);
        allocations
    });
    format!("creation: {} per derived node", per(allocations, COUNT))
}

fn main() {
    let rows: [String; 4] = [chain(), fan_out(), detached_write(), creation()];
    println!("Allocations: {}", rows.join("; "));
}
