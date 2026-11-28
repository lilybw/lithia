// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::{node, SourceNode};

fn main() {
    let _line: SourceNode<u32> = node!(__LITHIA_SITE.line);
}
