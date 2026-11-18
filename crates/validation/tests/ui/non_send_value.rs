// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

use std::rc::Rc;

use lithia::effect;

fn main() {
    let _ = effect!(Rc::new(1));
}
