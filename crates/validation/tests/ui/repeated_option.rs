// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::effect;

fn main() {
    let _ = effect!(0, ghost = ON_GHOST_EMIT_LATEST, ghost = ON_GHOST_SET_AS_PENDING);
}
