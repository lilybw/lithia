// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Diagnostics through `tracing` (spec §12).

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::LazyLock;
use std::time::Instant;

use crate::Site;
use crate::ids::RunId;
use crate::node::{Header, RateWindow};
use crate::site::SiteKey;
use crate::sync::MutexGuard;

/// Default rerun-rate threshold in runs per second; override with `LITHIA_RERUN_RATE`.
/// Milliseconds since the first diagnostic that needed the time.
pub(crate) type Millis = u64;

pub(crate) const DEFAULT_RERUN_RATE: u64 = 50_000;

/// Rerun-rate threshold, read from the environment once per process.
static RERUN_THRESHOLD: LazyLock<u64> = LazyLock::new(|| {
    std::env::var("LITHIA_RERUN_RATE")
        .ok()
        .and_then(|raw: String| raw.trim().parse::<u64>().ok())
        .filter(|rate: &u64| *rate > 0)
        .unwrap_or(DEFAULT_RERUN_RATE)
});

static START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Support-held nodes per creating site, for the growth warning.
static HELD: parking_lot::Mutex<BTreeMap<SiteKey, usize>> = parking_lot::Mutex::new(BTreeMap::new());

/// Sites already warned about an unread memo.
static UNREAD_WARNED: parking_lot::Mutex<BTreeSet<SiteKey>> = parking_lot::Mutex::new(BTreeSet::new());

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

/// Counts a run start and warns once per window when the node reruns too fast.
pub(crate) fn count_run(header: &Header) {
    let threshold: u64 = *RERUN_THRESHOLD;
    let now: Millis = millis();
    let observed: Option<u64> = {
        let mut guard: MutexGuard<'_, RateWindow> = header.rate.lock();
        let window: &mut RateWindow = &mut guard;
        if now.saturating_sub(window.start_ms) >= 1000 {
            window.start_ms = now;
            window.count = 0;
            window.warned = false;
        }
        window.count += 1;
        let first: bool = window.count > threshold && !window.warned;
        window.warned |= first;
        first.then_some(window.count)
    };
    if let Some(observed) = observed {
        emit(|| {
            tracing::warn!(
                site = %header.site,
                observed,
                threshold,
                "lithia: node reran faster than the configured rate; a loop may be unattributed"
            )
        });
    }
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
    let reached: usize = {
        let mut counts: parking_lot::MutexGuard<'_, BTreeMap<SiteKey, usize>> = HELD.lock();
        let count: &mut usize = counts.entry(site_key(site)).or_insert(0);
        *count += 1;
        *count
    };
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
    if let Some(count) = HELD.lock().get_mut(&site_key(site)) {
        *count = count.saturating_sub(1);
    }
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
