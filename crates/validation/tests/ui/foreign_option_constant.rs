// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::{effect, DerivedNode, SourceNode};

fn main() {
    let n: SourceNode<u32> = effect!(1);
    let _a: DerivedNode<u32> = effect!(|n| *n.get(), resilience = WHOLE);
    let _b: DerivedNode<u32> = effect!(|n| *n.get(), batches = lithia::resilience::NONE);
}
