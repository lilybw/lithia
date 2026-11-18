// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

use lithia::{effect, SourceNode};

#[lithia::main]
async fn main() {
    let pair: (u32, u32) = (1, 2);
    let _n: SourceNode<u32> = effect!(1, key = |(a, _b)| *a);
    let _ = pair;
}
