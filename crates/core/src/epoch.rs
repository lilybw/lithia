//! Scopes and the epoch tree with its progress accounting (spec §7.1, §11.3, §11.4 step 6).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::ids::{EpochId, next_epoch_id, next_scope_id};

/// Root of a subtree of epochs: a lithia task, or any other task or thread.
pub(crate) struct Scope {
    pub(crate) id: u64,
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
        if self.open.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.notify.notify_waiters();
        }
    }

    pub(crate) fn is_settled(&self) -> bool {
        self.open.load(Ordering::SeqCst) == 0
    }

    /// Waits until no epoch of this scope has open work.
    pub(crate) async fn wait_settled(&self) {
        loop {
            let notified = self.notify.notified();
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

/// A function waiting for an epoch to settle.
pub(crate) type SettleHook = Box<dyn FnOnce() + Send>;

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
        })
    }

    /// Counts one more open entry.
    pub(crate) fn open_token(&self) {
        self.own.fetch_add(1, Ordering::SeqCst);
        self.sub_inc();
    }

    /// Closes one open entry; the last one settles the epoch.
    pub(crate) fn close_token(&self) {
        if self.own.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.on_settled();
        }
        self.sub_dec();
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
        if self.sub.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.notify.notify_waiters();
            match &self.parent {
                Some(parent) => parent.sub_dec(),
                None => self.scope.dec(),
            }
        }
    }

    fn on_settled(&self) {
        let hooks: Vec<SettleHook> = {
            let mut guard = self.hooks.lock();
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
            let mut guard = self.hooks.lock();
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
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.sub.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }
}
