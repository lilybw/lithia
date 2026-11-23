// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Where runs execute: the fast path on the current thread, and the model executor under loom (spec §11.4).

pub(crate) mod fast_path;
#[cfg(lithia_loom)]
pub(crate) mod model;
pub(crate) mod spawn;
