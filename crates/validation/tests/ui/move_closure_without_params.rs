// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::effect;

fn main() {
    let x: i32 = 1;
    let _ = effect!(move || x);
}
