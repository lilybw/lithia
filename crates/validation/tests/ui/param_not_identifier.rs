// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::{node, DerivedNode, SourceNode};

fn main() {
    let pair: SourceNode<(i32, i32)> = node!((1, 2));
    let _sum: DerivedNode<i32> = node!(|(a, b)| a + b);
    let _ = pair;
}
