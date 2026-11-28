// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::{SourceNode, node};

fn main() {
    let _a: SourceNode<u32> = node!(0, no_equals, equals = lithia::eq);
    let _b: SourceNode<u32> = node!(0, no_equals = true);
    let _c: SourceNode<u32> = node!(0, equals);
}
