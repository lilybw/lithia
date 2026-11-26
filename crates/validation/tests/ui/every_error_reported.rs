// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::{node, SourceNode};

fn main() {
    let _n: SourceNode<u32> = node!(1, init = 0, resilience = NONE, bogus = 2);
}
