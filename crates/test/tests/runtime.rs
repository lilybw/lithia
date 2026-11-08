//! Runtimes, flavours and shutdown (spec §11.3).

use std::time::Duration;

use lithia::{DerivedNode, SourceNode, Status, effect};

#[test]
fn a_run_whose_runtime_shut_down_fails_its_node() {
    let runtime: tokio::runtime::Runtime = tokio::runtime::Runtime::new().unwrap();
    let (n, sleepy): (SourceNode<u32>, DerivedNode<u32>) = runtime.block_on(async {
        let n: SourceNode<u32> = effect!(1);
        let sleepy: DerivedNode<u32> = effect!(async |n| {
            tokio::time::sleep(Duration::from_secs(60)).await;
            *n.get()
        });
        tokio::time::sleep(Duration::from_millis(20)).await; // let the run start and suspend
        (n, sleepy)
    });
    drop(runtime);
    match lithia::status(&sleepy) {
        Status::Failed(failure) => assert_eq!(failure.message, "runtime shut down"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(*n.get(), 1);
}

#[test]
fn a_derived_node_needs_a_runtime() {
    let n: SourceNode<u32> = effect!(1); // source nodes need none
    let created: std::thread::Result<DerivedNode<u32>> =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| effect!(|n| *n.get())));
    assert!(created.is_err(), "as tokio::spawn would, creation without a runtime panics");
}

#[lithia::test(flavor = "current_thread")]
async fn the_current_thread_flavour_works() {
    let a: SourceNode<u32> = effect!(2);
    let squared: DerivedNode<u32> = effect!(|a| *a.get() * *a.get());
    lithia::flush().await;
    a.set(3);
    lithia::flush().await;
    assert_eq!(*squared.get(), 9);
}

#[lithia::test(flavor = "multi_thread", worker_threads = 1)]
async fn spawn_blocking_outside_bodies_delivers_in_the_caller_scope() {
    let a: SourceNode<u32> = effect!(0);
    let doubled: DerivedNode<u32> = effect!(|a| *a.get() * 2);
    let writer: SourceNode<u32> = a.clone();
    lithia::spawn_blocking(move || writer.set(21)).await.unwrap();
    lithia::flush().await;
    assert_eq!(*doubled.get(), 42);
}
