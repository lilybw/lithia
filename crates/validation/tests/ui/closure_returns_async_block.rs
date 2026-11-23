// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::{effect, DerivedNode, SourceNode};

fn main() {
    let n: SourceNode<u32> = effect!(1);
    let _later: DerivedNode<u32> = effect!(|n| async move { *n.get() });
}
