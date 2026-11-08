//! Diagnostics through `tracing` (spec §12).

use std::sync::OnceLock;
use std::time::Instant;

use crate::Site;
use crate::node::{Header, RateWindow};

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
    let mut window = header.rate.lock();
    let window: &mut RateWindow = &mut window;
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
        "lithia: derived node freed before its first run finished; the likely cause is `let _ = effect!(…)`"
    );
}

pub(crate) fn cut(writer: &Header, written: Option<&'static Site>) {
    let written: String = written.map_or_else(|| "<unknown>".to_string(), |site: &Site| site.to_string());
    if writer.is_unit {
        tracing::trace!(writer = %writer.site, written = %written, "lithia: a cycle cut an effect");
    } else {
        tracing::warn!(writer = %writer.site, written = %written, "lithia: a cycle cut a writer that holds a value");
    }
}

pub(crate) fn fragility_failed(site: &Site, parent: &str) {
    tracing::debug!(site = %site, parent, "lithia: child failed through fragility");
}

pub(crate) fn ghost_read(site: &Site, policy: crate::GhostPolicy) {
    tracing::debug!(site = %site, policy = ?policy, "lithia: read of a disposed node");
}

pub(crate) fn superseded(site: &Site, run: u64) {
    tracing::trace!(site = %site, run, "lithia: run superseded or aborted");
}

pub(crate) fn drop_panicked(message: &str) {
    tracing::warn!(message, "lithia: a dropped value panicked");
}
