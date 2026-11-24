// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Snapshots of `effect!`'s expansions, one per body form, in `snapshots/`; a change to an
//! expansion shows in review. `LITHIA_SNAPSHOTS=overwrite cargo test -p lithia-macro` rewrites them.

use std::fmt::Write as _;
use std::path::PathBuf;

use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};

const CASES: &[(&str, &str)] = &[
    ("source", "0"),
    ("source_options", "0, equals = |a: &u32, b: &u32| a == b, disposed = MASK"),
    ("source_keyed", "row.clone(), key = row.id"),
    ("async_source", "async { lookup().await }"),
    ("async_source_move", "async move { tx.send(1) }"),
    ("derived", "|n| *n.get() + offset"),
    ("derived_paths", "|n| self.total.set(*n.get() + self.base)"),
    ("derived_move", "move |n| tx.send(*n.get())"),
    ("derived_async", "async |n| { tokio::task::yield_now().await; *n.get() }"),
    ("derived_gen", "|n| -> u64 { yield u64::from(*n.get()); }"),
    ("derived_keyed", "|n| *n.get() * factor, key = id"),
    ("derived_options", "|n| *n.get(), batches = WHOLE, resilience = NONE"),
];

#[test]
fn expansions_match_their_snapshots() {
    let overwrite: bool = std::env::var("LITHIA_SNAPSHOTS").is_ok_and(|mode: String| mode == "overwrite");
    let dir: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("snapshots");
    let mut stale: Vec<&str> = Vec::new();
    for (name, input) in CASES {
        let parsed: crate::effect::EffectInput = syn::parse_str(input).expect("a snapshot case parses");
        let expansion: String = pretty(crate::effect::expand(parsed).expect("a snapshot case expands"));
        let text: String = format!("// effect!({input})\n\n{expansion}");
        let path: PathBuf = dir.join(format!("{name}.rs.txt"));
        if overwrite {
            std::fs::write(&path, &text).expect("the snapshot is written");
        } else if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            stale.push(name);
        }
    }
    assert!(stale.is_empty(), "expansions changed: {stale:?}; review them, then LITHIA_SNAPSHOTS=overwrite");
}

/// An expansion, one statement per line and indented by its blocks.
fn pretty(tokens: TokenStream) -> String {
    let mut out: String = String::new();
    write(tokens, 0, true, &mut out);
    let lines: Vec<&str> = out.lines().map(str::trim_end).filter(|line: &&str| !line.is_empty()).collect();
    lines.join("\n") + "\n"
}

/// Writes `tokens`; `block` when they are a block's statements, whose `;` ends a line.
fn write(tokens: TokenStream, depth: usize, block: bool, out: &mut String) {
    for tree in tokens {
        match tree {
            TokenTree::Group(group) if group.delimiter() == Delimiter::Brace => {
                let _ = write!(out, "{{\n{}", indent(depth + 1));
                write(group.stream(), depth + 1, true, out);
                let _ = write!(out, "\n{}}} ", indent(depth));
            }
            TokenTree::Group(group) => {
                let (open, close): (&str, &str) = match group.delimiter() {
                    Delimiter::Parenthesis => ("(", ")"),
                    Delimiter::Bracket => ("[", "]"),
                    Delimiter::Brace | Delimiter::None => ("", ""),
                };
                out.push_str(open);
                write(group.stream(), depth, false, out);
                let _ = write!(out, "{close} ");
            }
            TokenTree::Punct(punct) if punct.as_char() == ';' && block => {
                let _ = write!(out, ";\n{}", indent(depth));
            }
            TokenTree::Punct(punct) => {
                out.push(punct.as_char());
                if punct.spacing() == Spacing::Alone {
                    out.push(' ');
                }
            }
            other => {
                let _ = write!(out, "{other} ");
            }
        }
    }
}

fn indent(depth: usize) -> String {
    "    ".repeat(depth)
}
