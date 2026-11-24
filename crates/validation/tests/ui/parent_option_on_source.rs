// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::effect;

fn main() {
    let _ = effect!(0, resilience = NONE);
    let _ = effect!(async { 1 }, batches = WHOLE);
}
