// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Entry points of `#[lithia::main]` and `#[lithia::test]`: a runtime and a root lithia task.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::sync::oneshot;

/// Reads the test timeout the workspace harness sets, if any.
fn test_timeout() -> Option<Duration> {
    std::env::var("LITHIA_TEST_TIMEOUT_MS")
        .ok()
        .and_then(|raw: String| raw.trim().parse::<u64>().ok())
        .filter(|ms: &u64| *ms > 0)
        .map(Duration::from_millis)
}

/// Runs an entry future as a lithia task on a fresh runtime; used by `#[lithia::main]`.
#[doc(hidden)]
pub fn block_on_main<F: Future>(mut builder: tokio::runtime::Builder, future: F) -> F::Output {
    let runtime: tokio::runtime::Runtime =
        builder.enable_all().build().expect("lithia: cannot build the tokio runtime");
    runtime.block_on(crate::run::scoped(future))
}

/// Runs a test body as a lithia task on a fresh runtime, under the harness timeout if set; used by `#[lithia::test]`.
#[doc(hidden)]
pub fn block_on_test<F: Future>(mut builder: tokio::runtime::Builder, name: &str, future: F) -> F::Output {
    let runtime: tokio::runtime::Runtime =
        builder.enable_all().build().expect("lithia: cannot build the tokio runtime");
    let timeout: Option<Duration> = test_timeout();
    runtime.block_on(async move {
        let Some(limit) = timeout else {
            return crate::run::scoped(future).await;
        };
        let mut test: Pin<&mut crate::run::TaskFuture<F>> = std::pin::pin!(crate::run::scoped(future));
        let mut deadline: Option<oneshot::Receiver<()>> = Some(wall_clock(limit));
        std::future::poll_fn(|cx: &mut Context<'_>| {
            if let Poll::Ready(output) = test.as_mut().poll(cx) {
                return Poll::Ready(output);
            }
            if let Some(receiver) = deadline.as_mut()
                && let Poll::Ready(fired) = Pin::new(receiver).poll(cx)
            {
                if fired.is_ok() {
                    panic!("lithia: test `{name}` timed out after {limit:?}");
                }
                // The timer's thread is gone, so nothing times the test out.
                deadline = None;
            }
            Poll::Pending
        })
        .await
    })
}

/// Fires once `limit` of real time has passed, whatever the runtime's clock does, so a test whose
/// clock is paused still times out.
fn wall_clock(limit: Duration) -> oneshot::Receiver<()> {
    let (fire, fired): (oneshot::Sender<()>, oneshot::Receiver<()>) = oneshot::channel();
    std::thread::Builder::new()
        .name("lithia-test-timeout".into())
        .spawn(move || {
            std::thread::sleep(limit);
            let _ = fire.send(());
        })
        .expect("lithia: cannot start the test timeout's thread");
    fired
}

/// Pauses a runtime's clock at its start, for `start_paused`; tokio's clock control is behind
/// lithia's `test-util` feature.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`start_paused` needs lithia's `test-util` feature",
    label = "used here",
    note = "add `features = [\"test-util\"]` to the lithia dependency; it turns on tokio's `test-util`, which controls the clock"
)]
pub trait StartPaused {
    /// Sets the builder's `start_paused`.
    fn start_paused(builder: &mut tokio::runtime::Builder, paused: bool);
}

#[cfg(feature = "test-util")]
impl StartPaused for () {
    fn start_paused(builder: &mut tokio::runtime::Builder, paused: bool) {
        builder.start_paused(paused);
    }
}
