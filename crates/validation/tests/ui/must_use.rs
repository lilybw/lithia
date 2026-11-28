// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

// Case AH: a bare `node!(…).scoped();` is a `#[must_use]` warning.
#![deny(unused_must_use)]
use lithia::{node, SourceNode};

#[lithia::main]
async fn main() {
    let count: SourceNode<u32> = node!(0);
    node!(|count| println!("{}", count.get())).scoped(); // error: unused return value of `scoped`
}
