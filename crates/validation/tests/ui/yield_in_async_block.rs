// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::{node, DerivedNode, SourceNode};

fn main() {
    let n: SourceNode<u32> = node!(3);
    let _tick: DerivedNode<u32> = node!(async |n| {
        let inner = async {
            yield *n.get();
        };
        inner.await;
    });
}
