// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Entry points of `#[lithia::main]` and `#[lithia::test]`: a runtime and a root lithia task.

use std::future::Future;
use std::time::Duration;

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
pub fn block_on_test<F: Future<Output = ()>>(mut builder: tokio::runtime::Builder, name: &str, future: F) {
    let runtime: tokio::runtime::Runtime =
        builder.enable_all().build().expect("lithia: cannot build the tokio runtime");
    let timeout: Option<Duration> = test_timeout();
    runtime.block_on(async move {
        match timeout {
            Some(limit) => {
                if tokio::time::timeout(limit, crate::run::scoped(future)).await.is_err() {
                    panic!("lithia: test `{name}` timed out after {limit:?}");
                }
            }
            None => crate::run::scoped(future).await,
        }
    });
}
