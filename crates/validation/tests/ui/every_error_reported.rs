// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::{effect, SourceNode};

fn main() {
    let _n: SourceNode<u32> = effect!(1, init = 0, resilience = NONE, bogus = 2);
}
