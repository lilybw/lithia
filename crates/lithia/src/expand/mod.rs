// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! What `node!` expands to: node creation, keyed reclaim and capture recording (spec §4).

pub(crate) mod capture;
pub(crate) mod create;
pub(crate) mod entry;
pub(crate) mod keyed;
pub(crate) mod traits;
