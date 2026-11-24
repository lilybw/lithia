// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

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
    /// The last run panicked, or the node failed through its parents.
    Failed(Arc<Failure>),
    /// The node was freed by `lithia::dispose`.
    Ghost,
}

/// Why a node failed: its own panic, or its parents' failures under `resilience`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Failure {
    /// The panic message, or why its parents failed it.
    pub message: String,
    /// Where the failed node was created.
    pub site: Site,
    /// The failures of the parents that caused this one.
    pub causes: Vec<Arc<Failure>>,
}

impl Failure {
    /// A failure with no causes.
    pub(crate) fn new(message: impl Into<String>, site: Site) -> Failure {
        Failure { message: message.into(), site, causes: Vec::new() }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.site)
    }
}

/// The first cause is the source; the others are in `causes`.
impl std::error::Error for Failure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.causes.first().map(|cause: &Arc<Failure>| &**cause as &(dyn std::error::Error + 'static))
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
