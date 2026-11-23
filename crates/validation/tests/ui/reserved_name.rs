// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::{effect, SourceNode};

fn main() {
    let _line: SourceNode<u32> = effect!(__LITHIA_SITE.line);
}
