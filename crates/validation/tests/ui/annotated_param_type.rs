// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::{effect, DerivedNode, SourceNode};

fn main() {
    let count: SourceNode<u32> = effect!(1);
    let _wrong: DerivedNode<u32> = effect!(|count: SourceNode<i64>| 0);
}
