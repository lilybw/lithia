// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::{effect, DerivedNode, SourceNode};

fn main() {
    let n: SourceNode<u32> = effect!(1);
    let _items: DerivedNode<Vec<u32>> = effect!(|n| {
        let _ = vec![yield *n.get()];
    });
}
