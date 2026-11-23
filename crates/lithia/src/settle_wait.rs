// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Progress signalling for `lithia::test::settled` (spec §11.3).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use tokio::sync::Notify;

/// Read by every step on every core, so kept apart from what steps write.
static WAITERS: crate::sync::Padded<AtomicUsize> = crate::sync::Padded(AtomicUsize::new(0));
static SEQ: AtomicU64 = AtomicU64::new(0);
static NOTIFY: Notify = Notify::const_new();

/// Signals a change of scheduler state; free unless something waits on `settled`.
pub(crate) fn bump() {
    if WAITERS.load(Ordering::SeqCst) > 0 {
        SEQ.fetch_add(1, Ordering::SeqCst);
        NOTIFY.notify_waiters();
    }
}

/// Counts progress events; a waiter compares readings taken before and after it checks.
pub(crate) type SettleSeq = u64;

/// Registers a waiter until dropped.
pub(crate) struct Waiter(());

impl Waiter {
    pub(crate) fn new() -> Waiter {
        WAITERS.fetch_add(1, Ordering::SeqCst);
        Waiter(())
    }

    /// Current progress sequence number.
    pub(crate) fn seq(&self) -> SettleSeq {
        SEQ.load(Ordering::SeqCst)
    }

    /// The notifier signalled by every bump.
    pub(crate) fn notify(&self) -> &'static Notify {
        &NOTIFY
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        let before: usize = WAITERS.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(before > 0, "lithia: more settle waiters left than registered");
    }
}
