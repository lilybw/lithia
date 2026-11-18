// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Status, failures and the ghost error (spec §3.3, §3.4).

use std::fmt;
use std::sync::Arc;

use crate::Site;

/// Condition of a node as seen by readers and children.
/// `if let Status::Failed(f) = lithia::status(&parent) { eprintln!("{}", f.message); }`
#[derive(Clone, Debug)]
pub enum Status {
    /// The node holds its latest item and no run is pending.
    Ready,
    /// A run is in flight and has not produced an item yet.
    Pending,
    /// The last run panicked, or fragility failed the node.
    Failed(Arc<Failure>),
    /// The node was freed by `lithia::dispose`.
    Ghost,
}

/// Why a node failed: its own panic, or its parents' failures under `fragility`.
#[derive(Clone, Debug)]
pub struct Failure {
    /// The panic message, or the fragility reason.
    pub message: String,
    /// Where the failed node was created.
    pub site: Site,
    /// The failures of the parents that caused this one.
    pub causes: Vec<Arc<Failure>>,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.site)
    }
}

/// Error returned by the `try_` operations on a node freed by `lithia::dispose`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ghosted;

impl fmt::Display for Ghosted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the node was disposed")
    }
}

impl std::error::Error for Ghosted {}
