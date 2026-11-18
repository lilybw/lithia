// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

use lithia::effect;

fn main() {
    let a: i32 = 0;
    let _ = effect!(a = 3);
}
