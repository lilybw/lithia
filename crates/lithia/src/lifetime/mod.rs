// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! How long nodes live: operations, support and sweeps, disposal and teardown, and deferred drops (spec §10).

pub(crate) mod dispose;
pub(crate) mod drops;
pub(crate) mod op;
pub(crate) mod support;
pub(crate) mod teardown;
