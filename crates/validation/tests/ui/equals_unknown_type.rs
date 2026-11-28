// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

// A value type unknown where the node is created, which later turns out to lack `PartialEq`.
use lithia::node;

struct Connection;

fn main() {
    let pending = node!(None);
    pending.set(Some(Connection));
}
