// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

use lithia::{effect, SourceNode};

#[lithia::main]
async fn main() {
    let _n: SourceNode<u32> = effect!(1, key = 0.5f64);
    let weight: f64 = 0.5;
    let _m: SourceNode<u32> = effect!(1, key = |weight| *weight);
}
