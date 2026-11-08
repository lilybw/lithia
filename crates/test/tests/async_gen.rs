//! Async bodies, generators and supersession (spec §3.6, §7.3, §8).

use std::time::Duration;

use lithia::{Changes, DerivedNode, SourceNode, Status, effect};
use tokio::sync::mpsc;

#[lithia::test]
async fn rapid_input_supersedes_async_runs_and_settles_on_the_last() {
    let input: SourceNode<u32> = effect!(0);
    let completed: SourceNode<u32> = effect!(0);
    let slow: DerivedNode<u32> = effect!(async |input| {
        let value: u32 = *input.get();
        tokio::time::sleep(Duration::from_millis(5)).await;
        completed.update(|n: &u32| n + 1);
        value * 2
    });
    for value in 1..=20u32 {
        input.set(value);
        lithia::flush().await;
    }
    lithia::test::settled(&input).await;
    assert_eq!(*slow.get(), 40);
    assert!(*completed.get() < 21, "superseded runs were aborted: {}", *completed.get());
}

#[lithia::test]
async fn a_derived_rerun_replaces_a_running_generator() {
    let base: SourceNode<u32> = effect!(1);
    let ticks: DerivedNode<u32> = effect!(async |base| {
        let mut i: u32 = 0;
        loop {
            i += 1;
            yield *base.get() * 1000 + i;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    let mut stream: Changes<u32> = ticks.changes();
    while let Some(value) = stream.next().await {
        if value.as_ref() > &1002 {
            break;
        }
    }
    base.set(2);
    lithia::flush().await;
    while let Some(value) = stream.next().await {
        if *value > 2001 {
            break;
        }
    }
    let value: u32 = *ticks.get();
    assert!((2000..3000).contains(&value), "only the new generator publishes: {value}");
    lithia::dispose(&ticks);
}

#[lithia::test]
async fn an_async_source_generator_feeds_items() {
    let counter: SourceNode<u32> = effect!(async {
        for i in 1..=5u32 {
            yield i;
        }
    });
    let seen: SourceNode<u32> = effect!(0);
    let _sum: DerivedNode<()> = effect!(|counter| seen.update(|total: &u32| total.max(&*counter.get()).to_owned()));
    lithia::test::settled(&counter).await;
    assert_eq!(*counter.get(), 5);
    assert_eq!(*seen.get(), 5);
    assert!(matches!(lithia::status(&counter), Status::Ready));
}

#[lithia::test]
async fn a_stream_becomes_a_node_through_a_move_block() {
    let (tx, mut rx): (mpsc::Sender<u32>, mpsc::Receiver<u32>) = mpsc::channel(8);
    let latest: SourceNode<u32> = effect!(async move {
        while let Some(x) = rx.recv().await {
            yield x;
        }
    });
    for x in [3u32, 1, 4] {
        tx.send(x).await.unwrap();
    }
    drop(tx);
    lithia::test::settled(&latest).await;
    assert_eq!(*latest.get(), 4);
}

#[lithia::test]
async fn a_write_stops_a_source_fed_by_a_stream() {
    let (tx, mut rx): (mpsc::Sender<u32>, mpsc::Receiver<u32>) = mpsc::channel(8);
    let fed: SourceNode<u32> = effect!(async move {
        while let Some(x) = rx.recv().await {
            yield x;
        }
    });
    tx.send(1).await.unwrap();
    let mut stream: Changes<u32> = fed.changes();
    while let Some(value) = stream.next().await {
        if *value == 1 {
            break;
        }
    }
    fed.set(100); // a write is a run: it supersedes the block for good
    let _ = tx.send(2).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(*fed.get(), 100);
}

#[lithia::test]
async fn a_pending_parent_never_blocks_its_children() {
    let gate: SourceNode<u32> = effect!(0);
    let never: DerivedNode<u32> = effect!(async |gate| {
        let _ = *gate.get();
        std::future::pending::<()>().await;
        1
    }, init = 7);
    let child: DerivedNode<u32> = effect!(|never| *never.get() + 1);
    lithia::flush().await;
    assert!(matches!(lithia::status(&never), Status::Pending));
    assert_eq!(*child.get(), 8, "the child ran on the prior value");
    lithia::dispose(&never);
}

#[lithia::test]
async fn flush_waits_through_a_synchronous_generator() {
    let n: SourceNode<u32> = effect!(1000);
    let last: DerivedNode<u32> = effect!(|n| {
        for i in 1..=*n.get() {
            yield i;
        }
    });
    lithia::flush().await;
    assert_eq!(*last.get(), 1000);
}
