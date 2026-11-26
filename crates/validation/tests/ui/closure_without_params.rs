// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::node;

fn main() {
    let x: i32 = 1;
    let _ = node!(|| x);
}
