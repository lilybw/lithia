// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::node;

fn main() {
    let _ = node!(0, resilience = NONE);
    let _ = node!(async { 1 }, batches = WHOLE);
}
