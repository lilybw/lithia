// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::{effect, DerivedNode, SourceNode};

fn main() {
    let a: SourceNode<i32> = effect!(1);
    let _twice: DerivedNode<i32> = effect!(|a, a| *a.get());
}
