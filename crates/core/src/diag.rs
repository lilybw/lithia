//! Diagnostics through `tracing` (spec §12).

use std::sync::OnceLock;
use std::time::Instant;

use crate::Site;
use crate::node::{Header, RateWindow};
use crate::sync::MutexGuard;

/// Default rerun-rate threshold in runs per second; override with `LITHIA_RERUN_RATE`.
pub(crate) const DEFAULT_RERUN_RATE: u64 = 1000;

fn rerun_threshold() -> u64 {
    static THRESHOLD: OnceLock<u64> = OnceLock::new();
    *THRESHOLD.get_or_init(|| {
        std::env::var("LITHIA_RERUN_RATE")
            .ok()
            .and_then(|raw: String| raw.trim().parse::<u64>().ok())
            .filter(|rate: &u64| *rate > 0)
            .unwrap_or(DEFAULT_RERUN_RATE)
    })
}

fn millis() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Counts a run start and warns once per window when the node reruns too fast.
pub(crate) fn count_run(header: &Header) {
    let threshold: u64 = rerun_threshold();
    let now: u64 = millis();
    let mut guard: MutexGuard<'_, RateWindow> = header.rate.lock();
    let window: &mut RateWindow = &mut guard;
    if now.saturating_sub(window.start_ms) >= 1000 {
        window.start_ms = now;
        window.count = 0;
        window.warned = false;
    }
    window.count += 1;
    if window.count > threshold && !window.warned {
        window.warned = true;
        tracing::warn!(
            site = %header.site,
            observed = window.count,
            threshold,
            "lithia: node reran faster than the configured rate; a loop may be unattributed"
        );
    }
}

pub(crate) fn run_panicked(site: &Site, message: &str) {
    tracing::warn!(site = %site, message, "lithia: run panicked");
}

pub(crate) fn freed_before_first_run(site: &Site) {
    tracing::warn!(
        site = %site,
        "lithia: node freed before its first run got under way; nothing outside lithia held it or its parents"
    );
}

/// Support-held nodes per creating site, for the growth warning.
fn held_counts() -> &'static std::sync::Mutex<std::collections::HashMap<usize, usize>> {
    static COUNTS: OnceLock<std::sync::Mutex<std::collections::HashMap<usize, usize>>> = OnceLock::new();
    COUNTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Counts a node support now holds, warning at 1 000, 10 000, … nodes from one site.
pub(crate) fn support_held(site: &'static Site) {
    let key: usize = site as *const Site as usize;
    let mut counts = held_counts().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let count: &mut usize = counts.entry(key).or_insert(0);
    *count += 1;
    let reached: usize = *count;
    drop(counts);
    if reached >= 1000 && power_of_ten(reached) {
        tracing::warn!(
            site = %site,
            count = reached,
            "lithia: nodes held without handles keep piling up at this site; stop them with `.scoped()` or `lithia::dispose`"
        );
    }
}

/// Counts a node support let go of.
pub(crate) fn support_released(site: &'static Site) {
    let key: usize = site as *const Site as usize;
    let mut counts = held_counts().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(count) = counts.get_mut(&key) {
        *count = count.saturating_sub(1);
    }
}

/// Warns once per site about a memo support keeps running though nothing reads it.
pub(crate) fn unread_memo(site: &'static Site) {
    static WARNED: OnceLock<std::sync::Mutex<std::collections::HashSet<usize>>> = OnceLock::new();
    let key: usize = site as *const Site as usize;
    let first: bool = WARNED
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key);
    if first {
        tracing::warn!(
            site = %site,
            "lithia: a memo keeps running though nothing reads it; bind it with `.scoped()` or dispose it"
        );
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
    Superseded { site: &'static Site, run: u64 },
}

impl Diag {
    pub(crate) fn emit(self) {
        match self {
            Diag::Cut { writer, is_unit, written } => cut(writer, is_unit, written),
            Diag::FragilityFailed { site, parent } => fragility_failed(site, &parent),
            Diag::Superseded { site, run } => superseded(site, run),
        }
    }
}

fn cut(writer: &Site, is_unit: bool, written: Option<&'static Site>) {
    let written: String = written.map_or_else(|| "<unknown>".to_string(), |site: &Site| site.to_string());
    if is_unit {
        tracing::trace!(writer = %writer, written = %written, "lithia: a cycle cut an effect");
    } else {
        tracing::warn!(writer = %writer, written = %written, "lithia: a cycle cut a writer that holds a value");
    }
}

fn fragility_failed(site: &Site, parent: &str) {
    tracing::debug!(site = %site, parent, "lithia: child failed through fragility");
}

pub(crate) fn ghost_read(site: &Site, policy: crate::GhostPolicy) {
    tracing::debug!(site = %site, policy = ?policy, "lithia: read of a disposed node");
}

fn superseded(site: &Site, run: u64) {
    tracing::trace!(site = %site, run, "lithia: run superseded or aborted");
}

pub(crate) fn drop_panicked(message: &str) {
    tracing::warn!(message, "lithia: a dropped value panicked");
}
