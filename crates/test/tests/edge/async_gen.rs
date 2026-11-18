// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

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
    let never: DerivedNode<u32> = effect!(
        async |gate| {
            let _ = *gate.get();
            std::future::pending::<()>().await;
            1
        },
        init = 7
    );
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

#[lithia::test]
async fn settled_follows_a_write_along_an_edge() {
    let input: SourceNode<u32> = effect!(0);
    let mirror: SourceNode<u32> = effect!(0);
    let _writer: DerivedNode<()> = effect!(|input| mirror.set(*input.get()));
    let slow: DerivedNode<u32> = effect!(async |mirror| {
        let value: u32 = *mirror.get();
        tokio::time::sleep(Duration::from_millis(30)).await;
        value
    });
    lithia::test::settled(&slow).await;
    input.set(5);
    // `slow` is downstream of a node the writer writes, not of `input`.
    lithia::test::settled(&input).await;
    assert_eq!(*slow.get(), 5);
}

#[lithia::test]
async fn settled_follows_a_write_through_a_handle_read_from_a_value() {
    let input: SourceNode<u32> = effect!(0);
    let mirror: SourceNode<u32> = effect!(0);
    let slot: SourceNode<Option<SourceNode<u32>>> = effect!(Some(mirror.clone()));
    let _writer: DerivedNode<()> = effect!(|input, slot| {
        if let Some(target) = &*slot.get() {
            target.set(*input.get());
        }
    });
    let slow: DerivedNode<u32> = effect!(async |mirror| {
        let value: u32 = *mirror.get();
        tokio::time::sleep(Duration::from_millis(30)).await;
        value
    });
    lithia::test::settled(&slow).await;
    input.set(5);
    // The writer has no edge to `mirror`: its run noted the write.
    lithia::test::settled(&input).await;
    assert_eq!(*slow.get(), 5);
}

#[lithia::test]
async fn an_async_node_makes_progress_under_fast_input() {
    let input: SourceNode<u32> = effect!(0);
    let slow: DerivedNode<u32> = effect!(async |input| {
        let value: u32 = *input.get();
        tokio::time::sleep(Duration::from_millis(8)).await;
        value
    });
    let mut published: Vec<u32> = Vec::new();
    for value in 1..=40u32 {
        input.set(value);
        lithia::flush().await;
        tokio::time::sleep(Duration::from_millis(2)).await;
        published.push(*slow.get());
    }
    // A run that replaced another finishes before new input replaces it, so values come through
    // while input keeps outpacing the body.
    published.dedup();
    assert!(published.len() > 2, "{published:?}");
    lithia::test::settled(&slow).await;
    assert_eq!(*slow.get(), 40);
}

#[lithia::test]
async fn a_replacing_generator_is_superseded_after_its_first_item() {
    let input: SourceNode<u32> = effect!(0);
    let ticks: DerivedNode<u32> = effect!(|input| {
        let base: u32 = *input.get() * 1000;
        let mut n: u32 = 0;
        loop {
            yield base + n;
            n += 1;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    lithia::flush().await;
    input.set(1); // supersedes the first run: the replacement is protected until its first item
    lithia::flush().await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    input.set(2); // the replacement has yielded, so this supersedes it
    lithia::flush().await;
    let deadline: tokio::time::Instant = tokio::time::Instant::now() + Duration::from_secs(2);
    while *ticks.get() < 2000 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(*ticks.get() >= 2000, "{}", *ticks.get());
    lithia::dispose(&ticks);
}
