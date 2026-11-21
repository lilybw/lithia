// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

// Case AH: a bare `effect!(…).scoped();` is a `#[must_use]` warning.
#![deny(unused_must_use)]
use lithia::{effect, SourceNode};

#[lithia::main]
async fn main() {
    let count: SourceNode<u32> = effect!(0);
    effect!(|count| println!("{}", count.get())).scoped(); // error: unused return value of `scoped`
}
