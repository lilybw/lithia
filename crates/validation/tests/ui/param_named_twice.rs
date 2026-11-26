// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::{node, DerivedNode, SourceNode};

fn main() {
    let a: SourceNode<i32> = node!(1);
    let _twice: DerivedNode<i32> = node!(|a, a| *a.get());
}
