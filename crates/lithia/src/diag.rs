// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Diagnostics through `tracing` (spec §12).

use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use crate::Site;
use crate::graph::ids::RunId;
use crate::site::SiteKey;
use crate::sync::AtomicBool;

/// Default rerun-rate threshold in runs per second; override with `LITHIA_RERUN_RATE`.
/// Milliseconds since the first diagnostic that needed the time.
pub(crate) type Millis = u64;

pub(crate) const DEFAULT_RERUN_RATE: u64 = 50_000;

// Diagnostics' own state, below, orders nothing the protocol reads, so loom need not see it.

/// Rerun-rate threshold, read from the environment once per process.
#[allow(clippy::disallowed_types)]
static RERUN_THRESHOLD: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
    std::env::var("LITHIA_RERUN_RATE")
        .ok()
        .and_then(|raw: String| raw.trim().parse::<u64>().ok())
        .filter(|rate: &u64| *rate > 0)
        .unwrap_or(DEFAULT_RERUN_RATE)
});

#[allow(clippy::disallowed_types)]
static START: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

/// Sites already warned about an unread memo.
#[allow(clippy::disallowed_types)]
static UNREAD_WARNED: parking_lot::Mutex<BTreeSet<SiteKey>> = parking_lot::Mutex::new(BTreeSet::new());

/// Rerun counting for the rerun-rate warning (spec §12).
pub(crate) struct RateWindow {
    start_ms: Millis,
    count: u64,
    warned: bool,
}

/// Runs between clock reads while a window is under the threshold.
const CLOCK_EVERY: u64 = 1024;

impl RateWindow {
    pub(crate) const fn new() -> RateWindow {
        RateWindow { start_ms: 0, count: 0, warned: false }
    }

    /// Counts a run start; the count to warn with when it first passes the threshold within one
    /// second. The clock is read as a window opens, every `CLOCK_EVERY` runs, and past the threshold.
    pub(crate) fn count(&mut self) -> Option<u64> {
        self.count += 1;
        if self.count == 1 {
            self.start_ms = millis();
            return None;
        }
        let threshold: u64 = *RERUN_THRESHOLD;
        let past: bool = self.count > threshold && !self.warned;
        if (past || self.count.is_multiple_of(CLOCK_EVERY)) && millis().saturating_sub(self.start_ms) >= 1000 {
            // A new window opens with this run.
            *self = RateWindow { start_ms: millis(), count: 1, warned: false };
            return None;
        }
        self.warned |= past;
        past.then_some(self.count)
    }
}

/// What a node's diagnostics track.
pub(crate) struct DiagState {
    /// Warn if freed before its first run got under way.
    pub(crate) warn_first_run: bool,
    /// The first run finished, yielded an item or was polled a second time.
    pub(crate) under_way: AtomicBool,
}

impl DiagState {
    pub(crate) fn new(warn_first_run: bool) -> DiagState {
        DiagState { warn_first_run, under_way: AtomicBool::new(false) }
    }
}

/// Runs one `tracing` emission; diagnostics run inside protocol steps, so a panicking subscriber is contained.
fn emit(event: impl FnOnce()) {
    if let Err(panic) = catch_unwind(AssertUnwindSafe(event)) {
        // Nothing is left to report it through; a payload whose drop panics as well is leaked.
        if let Err(nested) = catch_unwind(AssertUnwindSafe(move || drop(panic))) {
            std::mem::forget(nested);
        }
    }
}

fn millis() -> Millis {
    START.elapsed().as_millis() as Millis
}

/// Identifies a site by its address; every site is a `static`.
fn site_key(site: &'static Site) -> SiteKey {
    std::ptr::from_ref(site).addr()
}

/// Warns that a node reran faster than the threshold; `observed` runs in its window so far.
pub(crate) fn rerun_too_fast(site: &Site, observed: u64) {
    let threshold: u64 = *RERUN_THRESHOLD;
    emit(|| {
        tracing::warn!(
            site = %site,
            observed,
            threshold,
            "lithia: node reran faster than the configured rate; a loop may be unattributed"
        )
    });
}

pub(crate) fn run_panicked(site: &Site, message: &str) {
    emit(|| tracing::warn!(site = %site, message, "lithia: run panicked"));
}

pub(crate) fn freed_before_first_run(site: &Site) {
    emit(|| {
        tracing::warn!(
            site = %site,
            "lithia: node freed before its first run got under way; nothing outside lithia held it or its parents"
        )
    });
}

/// Counts a node support now holds, warning at 1 000, 10 000, … nodes from one site.
pub(crate) fn support_held(site: &'static Site) {
    let reached: usize = site.held.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if reached >= 1000 && power_of_ten(reached) {
        emit(|| {
            tracing::warn!(
                site = %site,
                count = reached,
                "lithia: nodes held without handles keep piling up at this site; stop them with `.scoped()` or `lithia::dispose`"
            )
        });
    }
}

/// Counts a node support let go of.
pub(crate) fn support_released(site: &'static Site) {
    let _ = site.held.0.fetch_update(
        std::sync::atomic::Ordering::Relaxed,
        std::sync::atomic::Ordering::Relaxed,
        |count: usize| count.checked_sub(1),
    );
}

/// Warns once per site about a memo support keeps running though nothing reads it.
pub(crate) fn unread_memo(site: &'static Site) {
    let first: bool = UNREAD_WARNED.lock().insert(site_key(site));
    if first {
        emit(|| {
            tracing::warn!(
                site = %site,
                "lithia: a memo keeps running though nothing reads it; bind it with `.scoped()` or dispose it"
            )
        });
    }
}

/// Whether `n` is 1, 10, 100, …
fn power_of_ten(mut n: usize) -> bool {
    while n >= 10 && n.is_multiple_of(10) {
        n /= 10;
    }
    n == 1
}

/// A diagnostic gathered under a scheduling lock, emitted once it is released.
pub(crate) enum Diag {
    Cut { writer: &'static Site, is_unit: bool, written: Option<&'static Site> },
    FragilityFailed { site: &'static Site, parent: String },
    Superseded { site: &'static Site, run: RunId },
    WholeEpochsBound { site: &'static Site },
}

impl Diag {
    pub(crate) fn emit(self) {
        match self {
            Diag::Cut { writer, is_unit, written } => cut(writer, is_unit, written),
            Diag::FragilityFailed { site, parent } => fragility_failed(site, &parent),
            Diag::Superseded { site, run } => superseded(site, run),
            Diag::WholeEpochsBound { site } => whole_epochs_bound(site),
        }
    }
}

fn cut(writer: &Site, is_unit: bool, written: Option<&'static Site>) {
    let written: String = written.map_or_else(|| "<unknown>".to_string(), |site: &Site| site.to_string());
    emit(|| {
        if is_unit {
            tracing::debug!(writer = %writer, written = %written, "lithia: a cycle cut an effect");
        } else {
            tracing::warn!(writer = %writer, written = %written, "lithia: a cycle cut a writer that holds a value");
        }
    });
}

/// Sites already warned that a `WHOLE_EPOCHS` node ran before its epochs were whole.
#[allow(clippy::disallowed_types)]
static WHOLE_WARNED: parking_lot::Mutex<BTreeSet<SiteKey>> = parking_lot::Mutex::new(BTreeSet::new());

fn whole_epochs_bound(site: &'static Site) {
    if WHOLE_WARNED.lock().insert(site_key(site)) {
        emit(|| {
            tracing::warn!(
                site = %site,
                bound = crate::protocol::WHOLE_EPOCHS_BOUND,
                "lithia: a WHOLE_EPOCHS node ran before every epoch reached it whole; input never paused"
            )
        });
    }
}

fn fragility_failed(site: &Site, parent: &str) {
    emit(|| tracing::debug!(site = %site, parent, "lithia: child failed through fragility"));
}

/// A run created two nodes with equal keys at one site; they are matched in creation order.
pub(crate) fn duplicate_key(site: &Site) {
    emit(|| tracing::warn!(site = %site, "lithia: a run created two nodes with the same key at one site"));
}

/// A keyed node was created outside any run, where no earlier creation can be reclaimed.
pub(crate) fn key_outside_run(site: &Site) {
    emit(|| tracing::debug!(site = %site, "lithia: `key` has no effect outside a body; the node is created as usual"));
}

pub(crate) fn ghost_read(site: &Site, policy: crate::GhostPolicy) {
    emit(|| tracing::debug!(site = %site, policy = ?policy, "lithia: read of a disposed node"));
}

fn superseded(site: &Site, run: RunId) {
    emit(|| tracing::trace!(site = %site, run = %run, "lithia: run superseded or aborted"));
}

pub(crate) fn drop_panicked(message: &str) {
    emit(|| tracing::warn!(message, "lithia: a dropped value panicked"));
}

pub(crate) fn defer_panicked(message: &str) {
    emit(|| tracing::warn!(message, "lithia: a deferred function panicked"));
}

/// Counts of one support sweep.
pub(crate) fn sweep(raised: usize, walked: usize, released: usize) {
    emit(|| tracing::trace!(raised, walked, released, "lithia: sweep"));
}
