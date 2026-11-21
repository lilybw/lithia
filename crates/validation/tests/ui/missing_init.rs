// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::{effect, DerivedNode, SourceNode};

struct NoDefault(u32);

fn main() {
    let n: SourceNode<u32> = effect!(1);
    let _wrapped: DerivedNode<NoDefault> = effect!(|n| NoDefault(*n.get()));
}
