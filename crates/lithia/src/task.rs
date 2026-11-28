// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Handles to the tasks `lithia::spawn` and `lithia::spawn_blocking` start (spec §3.5).

use std::any::Any;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A task `lithia::spawn` or `lithia::spawn_blocking` started; awaiting it gives the task's output.
/// Dropping it lets the task run on. `let total: u64 = lithia::spawn_blocking(move || sum(&rows)).await?;`
pub struct JoinHandle<T> {
    inner: tokio::task::JoinHandle<T>,
}

impl<T> JoinHandle<T> {
    pub(crate) fn new(inner: tokio::task::JoinHandle<T>) -> JoinHandle<T> {
        JoinHandle { inner }
    }

    /// Cancels the task at its next await; blocking work that has started runs to its end.
    pub fn abort(&self) {
        self.inner.abort();
    }

    /// True once the task has ended: finished, panicked or cancelled.
    pub fn is_finished(&self) -> bool {
        self.inner.is_finished()
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<T, JoinError>> {
        Pin::new(&mut self.inner).poll(cx).map(|ended: Result<T, tokio::task::JoinError>| ended.map_err(JoinError))
    }
}

impl<T> fmt::Debug for JoinHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinHandle").field("finished", &self.inner.is_finished()).finish()
    }
}

/// Why a task gave no output: it panicked, or it was cancelled by `abort` or by its runtime shutting down.
pub struct JoinError(tokio::task::JoinError);

impl JoinError {
    /// True if the task was cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }

    /// True if the task panicked.
    pub fn is_panic(&self) -> bool {
        self.0.is_panic()
    }

    /// The task's panic payload, for `std::panic::resume_unwind`, or the error back if it was cancelled.
    pub fn try_into_panic(self) -> Result<Box<dyn Any + Send + 'static>, JoinError> {
        self.0.try_into_panic().map_err(JoinError)
    }
}

impl fmt::Debug for JoinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinError").field("cancelled", &self.0.is_cancelled()).finish()
    }
}

impl fmt::Display for JoinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_cancelled() { f.write_str("the task was cancelled") } else { f.write_str("the task panicked") }
    }
}

impl std::error::Error for JoinError {}
