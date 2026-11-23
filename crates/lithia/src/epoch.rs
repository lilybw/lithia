// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Scopes and the epoch tree with its progress accounting (spec §7.1, §11.3, §11.4 step 6).

use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};

use crate::sync::{AtomicUsize, Mutex, MutexGuard};
use tokio::sync::Notify;
use tokio::sync::futures::Notified;

use crate::ids::{EpochId, NodeId, ScopeId, WriterId, next_epoch_id, next_scope_id};

/// Root of a subtree of epochs: a lithia task, or any other task or thread.
pub(crate) struct Scope {
    pub(crate) id: ScopeId,
    /// Root epochs with open work, plus deferred functions not yet delivered.
    open: AtomicUsize,
    notify: Notify,
    /// Live nodes created in this scope.
    pub(crate) live: AtomicUsize,
    /// Most recent root epoch, which `defer` attaches to outside lithia tasks.
    last: Mutex<Weak<Epoch>>,
}

impl Scope {
    pub(crate) fn new() -> Arc<Scope> {
        Arc::new(Scope {
            id: next_scope_id(),
            open: AtomicUsize::new(0),
            notify: Notify::new(),
            live: AtomicUsize::new(0),
            last: Mutex::new(Weak::new()),
        })
    }

    pub(crate) fn inc(&self) {
        self.open.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn dec(&self) {
        let before: usize = self.open.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(before > 0, "lithia: scope {} returned a token it did not hold", self.id);
        if before == 1 {
            self.notify.notify_waiters();
        }
    }

    pub(crate) fn is_settled(&self) -> bool {
        self.open.load(Ordering::SeqCst) == 0
    }

    /// Waits until no epoch of this scope has open work.
    pub(crate) async fn wait_settled(&self) {
        loop {
            let notified: Notified<'_> = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_settled() {
                return;
            }
            notified.await;
        }
    }

    pub(crate) fn set_last(&self, epoch: &Arc<Epoch>) {
        *self.last.lock() = Arc::downgrade(epoch);
    }

    pub(crate) fn last(&self) -> Option<Arc<Epoch>> {
        self.last.lock().upgrade()
    }
}

/// An open-work token of a scope, returned when dropped; a panic cannot keep the scope from settling.
pub(crate) struct ScopeToken(Arc<Scope>);

impl ScopeToken {
    /// Takes a token from `scope`.
    pub(crate) fn take(scope: Arc<Scope>) -> ScopeToken {
        scope.inc();
        ScopeToken(scope)
    }

    pub(crate) fn scope(&self) -> &Arc<Scope> {
        &self.0
    }
}

impl Drop for ScopeToken {
    fn drop(&mut self) {
        self.0.dec();
    }
}

/// A function waiting for an epoch to settle.
pub(crate) type SettleHook = Box<dyn FnOnce() + Send>;
/// Epoch tokens a caller holds, one entry per token, closed with `close_all` once no node
/// reference is held.
pub(crate) type Tokens = Vec<Arc<Epoch>>;
/// Epochs referred to without holding a token.
pub(crate) type Epochs = Vec<Arc<Epoch>>;

struct Hooks {
    settled: bool,
    list: Vec<SettleHook>,
}

/// One propagation: an outside batch, or one later delivery of a run for one epoch it serves.
pub(crate) struct Epoch {
    pub(crate) id: EpochId,
    parent: Option<Arc<Epoch>>,
    pub(crate) scope: Arc<Scope>,
    /// Open entries of this epoch, including those runs hold.
    own: AtomicUsize,
    /// `own`, plus child epochs whose subtree has open work.
    sub: AtomicUsize,
    notify: Notify,
    hooks: Mutex<Hooks>,
    /// Writes runs delivered into this epoch, as (writer, written node), for cut reruns (spec §7.4).
    writes: Mutex<Vec<(WriterId, NodeId)>>,
}

impl Epoch {
    /// Opens a root epoch of `scope`, holding one guard token for the delivery.
    pub(crate) fn root(scope: &Arc<Scope>) -> Arc<Epoch> {
        scope.inc();
        Self::with(None, scope.clone())
    }

    /// Opens a child epoch of `parent`, holding one guard token for the delivery.
    pub(crate) fn child(parent: &Arc<Epoch>) -> Arc<Epoch> {
        parent.sub_inc();
        Self::with(Some(parent.clone()), parent.scope.clone())
    }

    fn with(parent: Option<Arc<Epoch>>, scope: Arc<Scope>) -> Arc<Epoch> {
        Arc::new(Epoch {
            id: next_epoch_id(),
            parent,
            scope,
            own: AtomicUsize::new(1),
            sub: AtomicUsize::new(1),
            notify: Notify::new(),
            hooks: Mutex::new(Hooks { settled: false, list: Vec::new() }),
            writes: Mutex::new(Vec::new()),
        })
    }

    /// Records that `writer`'s run wrote `written` in this epoch.
    pub(crate) fn record_write(&self, writer: WriterId, written: NodeId) {
        let mut guard: MutexGuard<'_, Vec<(WriterId, NodeId)>> = self.writes.lock();
        if !guard.contains(&(writer, written)) {
            guard.push((writer, written));
        }
    }

    /// The nodes `writer` wrote in this epoch and the epochs it descends from.
    pub(crate) fn written_by(&self, writer: WriterId) -> Vec<NodeId> {
        let mut out: Vec<NodeId> = Vec::new();
        let mut at: Option<&Epoch> = Some(self);
        while let Some(epoch) = at {
            for (by, written) in epoch.writes.lock().iter() {
                if *by == writer && !out.contains(written) {
                    out.push(*written);
                }
            }
            at = epoch.parent.as_deref();
        }
        out
    }

    /// Counts one more open entry.
    pub(crate) fn open_token(&self) {
        self.own.fetch_add(1, Ordering::SeqCst);
        self.sub_inc();
    }

    /// Closes one open entry; the last one settles the epoch.
    pub(crate) fn close_token(&self) {
        self.close_tokens(1);
    }

    /// Closes `count` open entries at once, as `count` closes in a row would.
    pub(crate) fn close_tokens(&self, count: usize) {
        let before: usize = self.own.fetch_sub(count, Ordering::SeqCst);
        debug_assert!(before >= count, "lithia: epoch {} closed {count} of its {before} tokens", self.id);
        if before == count {
            self.on_settled();
        }
        self.sub_dec_by(count);
    }

    fn sub_inc(&self) {
        if self.sub.fetch_add(1, Ordering::SeqCst) == 0 {
            match &self.parent {
                Some(parent) => parent.sub_inc(),
                None => self.scope.inc(),
            }
        }
    }

    fn sub_dec(&self) {
        self.sub_dec_by(1);
    }

    fn sub_dec_by(&self, count: usize) {
        let before: usize = self.sub.fetch_sub(count, Ordering::SeqCst);
        debug_assert!(before >= count, "lithia: epoch {} closed {count} of its {before} open subtrees", self.id);
        if before == count {
            self.notify.notify_waiters();
            match &self.parent {
                Some(parent) => parent.sub_dec(),
                None => self.scope.dec(),
            }
        }
    }

    fn on_settled(&self) {
        let hooks: Vec<SettleHook> = {
            let mut guard: MutexGuard<'_, Hooks> = self.hooks.lock();
            guard.settled = true;
            std::mem::take(&mut guard.list)
        };
        for hook in hooks {
            hook();
        }
    }

    /// Runs `hook` once this epoch's own entries have all closed, or at once if they have.
    pub(crate) fn on_settle(&self, hook: SettleHook) {
        let run_now: Option<SettleHook> = {
            let mut guard: MutexGuard<'_, Hooks> = self.hooks.lock();
            if guard.settled && self.own.load(Ordering::SeqCst) == 0 {
                Some(hook)
            } else {
                guard.list.push(hook);
                None
            }
        };
        if let Some(hook) = run_now {
            hook();
        }
    }

    /// Waits until this epoch and every child epoch have no open work.
    pub(crate) async fn wait_subtree(&self) {
        loop {
            let notified: Notified<'_> = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.sub.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }
}
