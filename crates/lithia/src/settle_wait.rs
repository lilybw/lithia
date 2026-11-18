// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Progress signalling for `lithia::test::settled` (spec §11.3).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use tokio::sync::Notify;

static WAITERS: AtomicUsize = AtomicUsize::new(0);
static SEQ: AtomicU64 = AtomicU64::new(0);
static NOTIFY: Notify = Notify::const_new();

/// Signals a change of scheduler state; free unless something waits on `settled`.
pub(crate) fn bump() {
    if WAITERS.load(Ordering::SeqCst) > 0 {
        SEQ.fetch_add(1, Ordering::SeqCst);
        NOTIFY.notify_waiters();
    }
}

/// Registers a waiter until dropped.
pub(crate) struct Waiter(());

impl Waiter {
    pub(crate) fn new() -> Waiter {
        WAITERS.fetch_add(1, Ordering::SeqCst);
        Waiter(())
    }

    /// Current progress sequence number.
    pub(crate) fn seq(&self) -> u64 {
        SEQ.load(Ordering::SeqCst)
    }

    /// The notifier signalled by every bump.
    pub(crate) fn notify(&self) -> &'static Notify {
        &NOTIFY
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        WAITERS.fetch_sub(1, Ordering::SeqCst);
    }
}
