// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

use lithia::{effect, DerivedNode, SourceNode};

struct Token;

impl Token {
    fn id(&self) -> u32 {
        1
    }
}

fn main() {
    let n: SourceNode<u32> = effect!(1);
    let token: Token = Token;
    let _uses: DerivedNode<u32> = effect!(|n| *n.get() + token.id());
}
