// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

use lithia::effect;

fn main() {
    let x: i32 = 1;
    let _ = effect!(move || x);
}
