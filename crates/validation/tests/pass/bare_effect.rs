// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

// Case AH: a bare `node!(…);` statement compiles without warnings.
#![deny(warnings)]
use lithia::{node, SourceNode};

#[lithia::main]
async fn main() {
    let count: SourceNode<u32> = node!(0);
    node!(|count| println!("{}", count.get()));
    node!(async |count| println!("{}", count.get()));
    node!(async |count| loop {
        tokio::task::yield_now().await;
        let _ = count.get();
    });
    lithia::flush().await;
}
