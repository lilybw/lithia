// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

use lithia::{node, DerivedNode, SourceNode};

struct Token;

impl Token {
    fn id(&self) -> u32 {
        1
    }
}

fn main() {
    let n: SourceNode<u32> = node!(1);
    let token: Token = Token;
    let _uses: DerivedNode<u32> = node!(|n| *n.get() + token.id());
}
