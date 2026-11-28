// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Helpers shared by lithia's test crates: one tracing collector per binary, and polling waits.
//! The edge-case tests themselves live in `tests/`.

use std::time::Duration;

pub mod events;

pub use events::{Captured, events, find, reported, warned};

/// Waits until `done` holds, polling every millisecond.
pub async fn until(mut done: impl FnMut() -> bool) {
    while !done() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}
