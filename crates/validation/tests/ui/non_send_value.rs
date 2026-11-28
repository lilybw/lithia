// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use std::rc::Rc;

use lithia::node;

fn main() {
    let _ = node!(Rc::new(1));
}
