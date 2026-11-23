// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Tasks lithia roots its own scope in, whose polls form batches (spec §11.2).

use super::*;

/// A task started by `lithia::spawn`, `#[lithia::main]` or `#[lithia::test]`: each poll is a batch.
pub struct TaskFuture<F> {
    pub(super) scope: Arc<Scope>,
    pub(super) future: Pin<Box<F>>,
}

impl<F: Future> TaskFuture<F> {
    pub(crate) fn new(scope: Arc<Scope>, future: F) -> TaskFuture<F> {
        TaskFuture { scope, future: Box::pin(future) }
    }
}

impl<F: Future> Future for TaskFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let this: &mut TaskFuture<F> = self.get_mut();
        let frame: Frame = Frame::new(Target::Scope(this.scope.clone()));
        struct Deliver<'a> {
            frame: &'a Frame,
            scope: &'a Arc<Scope>,
        }
        impl Drop for Deliver<'_> {
            fn drop(&mut self) {
                let batch: Batch = self.frame.take_batch();
                protocol::deliver_outside(batch, self.scope);
            }
        }
        let deliver: Deliver<'_> = Deliver { frame: &frame, scope: &this.scope };
        let polled: Poll<F::Output> = context::entered(&frame, || this.future.as_mut().poll(cx));
        drop(deliver);
        polled
    }
}

/// Starts a lithia task in a new scope; its polls form batches (spec §3.5).
pub(crate) fn scoped<F: Future>(future: F) -> TaskFuture<F> {
    TaskFuture::new(Scope::new(), future)
}

#[doc(hidden)]
impl<F> std::fmt::Debug for TaskFuture<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskFuture").field("scope", &self.scope.id).finish()
    }
}
