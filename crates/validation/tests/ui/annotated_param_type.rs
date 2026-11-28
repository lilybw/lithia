// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::{node, DerivedNode, SourceNode};

fn main() {
    let count: SourceNode<u32> = node!(1);
    let _wrong: DerivedNode<u32> = node!(|count: SourceNode<i64>| 0);
}
