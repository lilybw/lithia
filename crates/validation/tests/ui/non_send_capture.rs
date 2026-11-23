// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use std::rc::Rc;

use lithia::{effect, DerivedNode, SourceNode};

fn main() {
    let n: SourceNode<u32> = effect!(1);
    let shared: Rc<u32> = Rc::new(2);
    let _uses: DerivedNode<u32> = effect!(|n| *n.get() + **shared);
}
