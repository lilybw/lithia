// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

use lithia::{effect, DerivedNode, SourceNode};

fn main() {
    let a: SourceNode<i32> = effect!(1);
    let _twice: DerivedNode<i32> = effect!(|a, a| *a.get());
}
