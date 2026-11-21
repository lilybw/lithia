// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use std::rc::Rc;

use lithia::effect;

fn main() {
    let _ = effect!(Rc::new(1));
}
