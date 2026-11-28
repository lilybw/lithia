// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::{node, DerivedNode, SourceNode};

struct NoDefault(u32);

fn main() {
    let n: SourceNode<u32> = node!(1);
    let _wrapped: DerivedNode<NoDefault> = node!(|n| NoDefault(*n.get()));
}
