// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Sites and tracing diagnostics (spec §4.3, §12).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lithia::{Changes, DerivedNode, Site, SourceNode, Status, node};
use tracing::Level;

use lithia_test::{Captured, events, find};

fn site_of(status: Status) -> Option<Site> {
    match status {
        Status::Failed(failure) => Some(failure.site),
        _ => None,
    }
}

#[lithia::test]
async fn a_site_records_where_the_node_was_created() {
    let n: SourceNode<u32> = node!(1);
    let line: u32 = line!() + 1;
    let failing: DerivedNode<u32> = node!(|n| {
        if *n.get() > 0 {
            panic!("site probe")
        }
        0
    });
    lithia::flush().await;
    let site: Site = site_of(lithia::status(&failing)).expect("the body panicked");
    assert!(site.file.ends_with("diagnostics.rs"), "{}", site.file);
    assert_eq!(site.line, line);
    assert!(site.module.ends_with("diagnostics"), "{}", site.module);
    assert!((site.function)().contains("a_site_records_where_the_node_was_created"), "{}", (site.function)());
    // A release build keeps no body source.
    assert_eq!(site.tokens.contains("site probe"), cfg!(debug_assertions), "{}", site.tokens);
}

#[lithia::test]
async fn a_panicking_run_warns_with_its_site_and_message() {
    let _ = events();
    let n: SourceNode<u32> = node!(0);
    let _boom: DerivedNode<u32> = node!(|n| if *n.get() == 1 { panic!("warn-probe-7781") } else { 0 });
    lithia::flush().await;
    n.set(1);
    lithia::flush().await;
    let event: Option<Captured> =
        find(Level::WARN, &["run panicked", "warn-probe-7781", "diagnostics.rs"], Duration::from_secs(2)).await;
    assert!(event.is_some());
}

#[lithia::test]
async fn a_node_freed_before_its_first_run_warns() {
    let _ = events();
    let line: String = format!("diagnostics.rs:{}", line!() + 1);
    let _ = node!(async { 7782u32 });
    lithia::flush().await;
    let event: Option<Captured> =
        find(Level::WARN, &["freed before its first run", &line], Duration::from_secs(2)).await;
    assert!(event.is_some());
}

#[lithia::test]
async fn cuts_warn_for_value_holders_and_debug_for_effects() {
    let _ = events();
    let x: SourceNode<i32> = node!(0);
    let line: String = format!("diagnostics.rs:{}", line!() + 1);
    let clamp: DerivedNode<bool> = node!(|x| {
        let over: bool = *x.get() > 10;
        if over {
            x.set(10);
        }
        over
    });
    x.set(15);
    lithia::flush().await;
    assert_eq!(*x.get(), 10);
    let event: Option<Captured> =
        find(Level::WARN, &["cut a writer that holds a value", &line], Duration::from_secs(2)).await;
    assert!(event.is_some());
    drop(clamp);

    let y: SourceNode<i32> = node!(0);
    let line: String = format!("diagnostics.rs:{}", line!() + 1);
    let _effect: DerivedNode<()> = node!(|y| if *y.get() > 10 {
        y.set(10);
    });
    y.set(15);
    lithia::flush().await;
    let event: Option<Captured> = find(Level::DEBUG, &["cut an effect", &line], Duration::from_secs(2)).await;
    assert!(event.is_some());
}

#[lithia::test]
async fn an_unattributed_loop_triggers_the_rerun_rate_warning() {
    let _ = events();
    let n: SourceNode<u32> = node!(0);
    // A task the body spawns writes with an empty writer set, so nothing attributes this loop.
    let line: String = format!("diagnostics.rs:{}", line!() + 1);
    let holder: DerivedNode<()> = node!(|n| {
        let again: SourceNode<u32> = (*n).clone();
        lithia::spawn(async move { again.update(|v: &u32| v + 1) });
    });
    let _ = tokio::time::timeout(Duration::from_millis(100), lithia::flush()).await;
    let event: Option<Captured> =
        find(Level::WARN, &["reran faster than the configured rate", &line], Duration::from_secs(5)).await;
    lithia::dispose(&holder);
    assert!(event.is_some(), "loop count {}", *n.get());
}

/// A `batches = WHOLE` join whose input never pauses runs on the ready epochs once 64 of them have
/// become ready while it waited, and warns once.
#[lithia::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_whole_batches_join_runs_once_input_has_kept_it_waiting_too_long() {
    let _ = events();
    let a: SourceNode<u32> = node!(0);
    let b: SourceNode<u32> = node!(0);
    // `slow` holds `b`'s epoch at the gate, so the join keeps waiting for it.
    let (open, gate): (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>) = std::sync::mpsc::channel();
    let gate: Arc<Mutex<std::sync::mpsc::Receiver<()>>> = Arc::new(Mutex::new(gate));
    let slow: DerivedNode<u32> = node!(|b| {
        let value: u32 = *b.get();
        if value > 0 {
            // Blocking hands the worker's queue on, or a run tokio queued behind this one waits too.
            tokio::task::block_in_place(|| {
                let _ = gate.lock().unwrap().recv();
            });
        }
        value * 10
    });
    let runs: Arc<std::sync::atomic::AtomicU32> = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let line: String = format!("diagnostics.rs:{}", line!() + 1);
    let join: DerivedNode<u32> = node!(
        |a, slow| {
            runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *a.get() + *slow.get()
        },
        batches = WHOLE
    );
    lithia::flush().await;
    let before: u32 = runs.load(std::sync::atomic::Ordering::SeqCst);
    b.set(1);
    tokio::task::yield_now().await;
    // Each write is an epoch of its own that becomes ready at the join while `b`'s waits.
    for value in 1..64u32 {
        a.set(value);
        tokio::task::yield_now().await;
    }
    assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), before, "ran before the bound");
    a.set(64);
    tokio::task::yield_now().await;
    let deadline: tokio::time::Instant = tokio::time::Instant::now() + Duration::from_secs(5);
    while runs.load(std::sync::atomic::Ordering::SeqCst) == before && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(*join.get(), 64, "ran on the ready epochs, without `b`'s");
    let event: Option<Captured> = find(Level::WARN, &["batches = WHOLE", &line], Duration::from_secs(2)).await;
    assert!(event.is_some());
    open.send(()).unwrap();
    lithia::flush().await;
    assert_eq!(*join.get(), 74);
}

const UNREAD: &str = "nothing reads it";

#[lithia::test]
async fn a_memo_that_loses_its_last_child_warns_that_nothing_reads_it() {
    let _ = events();
    let input: SourceNode<u32> = node!(0);
    let line: String = format!("diagnostics.rs:{}", line!() + 1);
    let memo: DerivedNode<u32> = node!(|input| *input.get() + 1);
    let reader: DerivedNode<()> = node!(|memo| {
        let _ = memo.get();
    });
    lithia::flush().await;
    // Support keeps the memo for its reader: no warning.
    drop(memo);
    lithia::flush().await;
    assert!(find(Level::WARN, &[UNREAD, &line], Duration::from_millis(50)).await.is_none());
    // Its last reader goes without a sweep reaching the memo.
    lithia::dispose(&reader);
    assert!(find(Level::WARN, &[UNREAD, &line], Duration::from_secs(2)).await.is_some());
    drop(input);
}

#[lithia::test]
async fn a_memo_that_loses_its_last_stream_warns_that_nothing_reads_it() {
    let _ = events();
    let input: SourceNode<u32> = node!(0);
    let line: String = format!("diagnostics.rs:{}", line!() + 1);
    let memo: DerivedNode<u32> = node!(|input| *input.get() + 1);
    let stream: Changes<u32> = memo.changes();
    lithia::flush().await;
    drop(memo);
    lithia::flush().await;
    assert!(find(Level::WARN, &[UNREAD, &line], Duration::from_millis(50)).await.is_none());
    drop(stream);
    assert!(find(Level::WARN, &[UNREAD, &line], Duration::from_secs(2)).await.is_some());
    drop(input);
}

#[lithia::test]
async fn a_memo_that_loses_its_last_child_by_edge_alone_warns_too() {
    let _ = events();
    let input: SourceNode<u32> = node!(0);
    let line: String = format!("diagnostics.rs:{}", line!() + 1);
    let memo: DerivedNode<u32> = node!(|input| *input.get() + 1);
    // Its body never reads the memo, so only the edge makes it a reader.
    let child: DerivedNode<()> = node!(|memo| {});
    lithia::flush().await;
    drop(memo);
    lithia::flush().await;
    assert!(find(Level::WARN, &[UNREAD, &line], Duration::from_millis(50)).await.is_none());
    lithia::dispose(&child);
    assert!(find(Level::WARN, &[UNREAD, &line], Duration::from_secs(2)).await.is_some());
    drop(input);
}

/// Panics on every event, as a broken subscriber would.
struct PanickingLayer;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for PanickingLayer {
    fn on_event(&self, _event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        panic!("subscriber panics");
    }
}

#[lithia::test(flavor = "current_thread")]
async fn a_panicking_subscriber_leaves_lithia_working() {
    use tracing_subscriber::layer::SubscriberExt;
    let subscriber: tracing_subscriber::layer::Layered<PanickingLayer, tracing_subscriber::Registry> =
        tracing_subscriber::registry().with(PanickingLayer);
    let _default: tracing::subscriber::DefaultGuard = tracing::subscriber::set_default(subscriber);
    let n: SourceNode<u32> = node!(0);
    let checked: DerivedNode<u32> = node!(|n| {
        let value: u32 = *n.get();
        assert_ne!(value, 1, "the body refuses 1");
        value
    });
    lithia::flush().await;
    n.set(1); // the run panics, and so does the subscriber its warning reaches
    let flushed: Result<(), tokio::time::error::Elapsed> =
        tokio::time::timeout(Duration::from_secs(2), lithia::flush()).await;
    assert!(flushed.is_ok(), "flush hung after the subscriber panicked");
    assert!(matches!(lithia::status(&checked), Status::Failed(_)));
    n.set(2);
    lithia::flush().await;
    assert_eq!(*checked.get(), 2);
}

/// Panics when dropped, as state holding a broken resource might.
struct PanicsOnDrop;

impl Drop for PanicsOnDrop {
    fn drop(&mut self) {
        panic!("drop-probe-7783");
    }
}

#[lithia::test]
async fn a_comparator_whose_drop_panics_is_reported_not_raised() {
    let _ = events();
    let input: SourceNode<u32> = node!(1);
    let state: PanicsOnDrop = PanicsOnDrop;
    let node: DerivedNode<u32> = node!(
        |input| *input.get(),
        equals = move |a: &u32, b: &u32| {
            let _held: &PanicsOnDrop = &state;
            a == b
        }
    );
    lithia::flush().await;
    lithia::dispose(&node);
    drop(node);
    input.set(2);
    let flushed: Result<(), tokio::time::error::Elapsed> =
        tokio::time::timeout(Duration::from_secs(2), lithia::flush()).await;
    assert!(flushed.is_ok());
    let event: Option<Captured> =
        find(Level::WARN, &["a dropped value panicked", "drop-probe-7783"], Duration::from_secs(2)).await;
    assert!(event.is_some(), "the panic is reported from the drop thread");
}
