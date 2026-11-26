// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::{node, DerivedNode, SourceNode};

fn main() {
    let n: SourceNode<u32> = node!(1);
    let _later: DerivedNode<u32> = node!(|n| async move { *n.get() });
}
