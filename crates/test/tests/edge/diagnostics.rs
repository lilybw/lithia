// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Sites and tracing diagnostics (spec §4.3, §12).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lithia::{Changes, DerivedNode, Site, SourceNode, Status, effect};
use tracing::Level;

use crate::events::{Captured, events};

/// Waits up to `limit` for an event at `level` whose text contains every needle.
async fn find(level: Level, needles: &[&str], limit: Duration) -> Option<Captured> {
    let events: Arc<Mutex<Vec<Captured>>> = events();
    let deadline: tokio::time::Instant = tokio::time::Instant::now() + limit;
    loop {
        let found: Option<Captured> = events
            .lock()
            .unwrap()
            .iter()
            .find(|e: &&Captured| e.level == level && needles.iter().all(|n: &&str| e.text.contains(n)))
            .cloned();
        if found.is_some() || tokio::time::Instant::now() >= deadline {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn site_of(status: Status) -> Option<Site> {
    match status {
        Status::Failed(failure) => Some(failure.site),
        _ => None,
    }
}

#[lithia::test]
async fn a_site_records_where_the_node_was_created() {
    let n: SourceNode<u32> = effect!(1);
    let line: u32 = line!() + 1;
    let failing: DerivedNode<u32> = effect!(|n| {
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
    assert!(site.tokens.contains("site probe"), "{}", site.tokens);
}

#[lithia::test]
async fn a_panicking_run_warns_with_its_site_and_message() {
    let _ = events();
    let n: SourceNode<u32> = effect!(0);
    let _boom: DerivedNode<u32> = effect!(|n| if *n.get() == 1 { panic!("warn-probe-7781") } else { 0 });
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
    let _ = effect!(async { 7782u32 });
    lithia::flush().await;
    let event: Option<Captured> =
        find(Level::WARN, &["freed before its first run", &line], Duration::from_secs(2)).await;
    assert!(event.is_some());
}

#[lithia::test]
async fn cuts_warn_for_value_holders_and_debug_for_effects() {
    let _ = events();
    let x: SourceNode<i32> = effect!(0);
    let line: String = format!("diagnostics.rs:{}", line!() + 1);
    let clamp: DerivedNode<bool> = effect!(|x| {
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

    let y: SourceNode<i32> = effect!(0);
    let line: String = format!("diagnostics.rs:{}", line!() + 1);
    let _effect: DerivedNode<()> = effect!(|y| if *y.get() > 10 {
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
    let n: SourceNode<u32> = effect!(0);
    let line: String = format!("diagnostics.rs:{}", line!() + 1);
    let holder: DerivedNode<Option<DerivedNode<()>>> = effect!(|n| Some(effect!(|n| n.update(|v: &u32| v + 1))));
    let _ = tokio::time::timeout(Duration::from_millis(100), lithia::flush()).await;
    let event: Option<Captured> =
        find(Level::WARN, &["reran faster than the configured rate", &line], Duration::from_secs(5)).await;
    lithia::dispose(&holder);
    assert!(event.is_some(), "loop count {}", *n.get());
}

const UNREAD: &str = "nothing reads it";

#[lithia::test]
async fn a_memo_that_loses_its_last_child_warns_that_nothing_reads_it() {
    let _ = events();
    let input: SourceNode<u32> = effect!(0);
    let line: String = format!("diagnostics.rs:{}", line!() + 1);
    let memo: DerivedNode<u32> = effect!(|input| *input.get() + 1);
    let reader: DerivedNode<()> = effect!(|memo| {
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
    let input: SourceNode<u32> = effect!(0);
    let line: String = format!("diagnostics.rs:{}", line!() + 1);
    let memo: DerivedNode<u32> = effect!(|input| *input.get() + 1);
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
    let input: SourceNode<u32> = effect!(0);
    let line: String = format!("diagnostics.rs:{}", line!() + 1);
    let memo: DerivedNode<u32> = effect!(|input| *input.get() + 1);
    // Its body never reads the memo, so only the edge makes it a reader.
    let child: DerivedNode<()> = effect!(|memo| {});
    lithia::flush().await;
    drop(memo);
    lithia::flush().await;
    assert!(find(Level::WARN, &[UNREAD, &line], Duration::from_millis(50)).await.is_none());
    lithia::dispose(&child);
    assert!(find(Level::WARN, &[UNREAD, &line], Duration::from_secs(2)).await.is_some());
    drop(input);
}
