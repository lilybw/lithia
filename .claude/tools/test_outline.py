# SPDX-FileCopyrightText: 2026 lithia Authors
# SPDX-License-Identifier: MPL-2.0

"""Tests of outline.py on the Rust, Python and TLA+ forms that trip naive parsers."""

from __future__ import annotations

import unittest

from outline import outline_python, outline_rust, outline_tla

RUST: str = r'''//! Module doc.

// SPDX-style header, separated by a blank line: dropped.

use std::fmt;
pub use crate::a::{b, c};

/// A value.
#[derive(Clone)]
#[inline]
pub struct Point<T> {
    /// X.
    pub x: T,
    y: T,
}

pub struct Unit(pub u32, u8);

// Plain comment right above: kept.
pub(crate) const LIMIT: usize = { let x: usize = 3; x * 2 };

fn tricky<'a, F: Fn(u32) -> u32>(s: &'a str, f: F) -> impl Fn() -> &'a str where F: Send {
    let quote: char = '"';
    let brace: char = '{';
    let escaped: char = '\'';
    let raw: &str = r#"} fn hidden() {"#;
    fn nested() {}
    s
}

pub trait Shape: Send {
    /// Area.
    fn area(&self) -> f64;
    fn name(&self) -> String { String::new() }
}

impl<T> Point<T> {
    pub fn new(x: T, y: T) -> Self { Point { x, y } } // trailing note: dropped
    fn private(&self) {}
}

impl fmt::Debug for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { Ok(()) }
}

macro_rules! twice { ($e:expr) => { $e; $e }; }
twice!(println!("x"));

#[cfg(test)]
mod tests {
    #[test]
    fn works() {}
}

unsafe fn raw(p: *mut u8) -> *const u8 { p }
'''


class RustOutline(unittest.TestCase):
    def test_items_signatures_and_docs(self) -> None:
        out: str = "\n".join(outline_rust(RUST, False))
        expected: list[str] = [
            "    1 //! Module doc.",
            "    6 pub use crate::a::{b, c}",
            "    8 /// A value.",
            "   11 pub struct Point<T> {",
            "   13   pub x: T,",
            "   17 pub struct Unit(pub u32, u8)",
            "   19 // Plain comment right above: kept.",
            "   20 pub(crate) const LIMIT: usize",
            "   22 fn tricky<'a, F: Fn(u32) -> u32>(s: &'a str, f: F) -> impl Fn() -> &'a str where F: Send",
            "   31 pub trait Shape: Send",
            "   33   fn area(&self) -> f64",
            "   34   fn name(&self) -> String",
            "   37 impl<T> Point<T>",
            "   38   pub fn new(x: T, y: T) -> Self",
            "   42 impl fmt::Debug for Unit",
            "   46 macro_rules! twice",
            "   47 twice!(println!(\"x\"))",
            "   50 mod tests",
            "   52   fn works()",
        ]
        for line in expected:
            self.assertIn(line, out)
        for absent in ("SPDX", "#[inline]", "nested", "hidden", "trailing note", "use std::fmt"):
            self.assertNotIn(absent, out)

    def test_raw_pointers(self) -> None:
        self.assertIn("   55 unsafe fn raw(p: *mut u8) -> *const u8", outline_rust(RUST, False))

    def test_attribute_line_numbers(self) -> None:
        lines: list[str] = outline_rust(RUST, False)
        self.assertIn("    9 #[derive(Clone)]", lines)
        self.assertIn("   51   #[test]", lines)

    def test_public_view(self) -> None:
        out: str = "\n".join(outline_rust(RUST, True))
        for present in ("pub struct Point", "pub trait Shape", "fn area", "pub fn new", "impl fmt::Debug for Unit"):
            self.assertIn(present, out)
        for absent in ("LIMIT", "tricky", "fn private", "macro_rules", "fn works"):
            self.assertNotIn(absent, out)


class PythonOutline(unittest.TestCase):
    def test_functions_classes_and_annotations(self) -> None:
        src: str = '"""Doc."""\nX: int = 1\nclass A(B):\n    y: str\n    def f(self, a: int) -> str:\n        """F."""\n'
        self.assertEqual(
            outline_python(src, False),
            ['    1 """Doc."""', "    2 X: int", "    3 class A(B)", "    4   y: str",
             "    5   def f(self, a: int) -> str", '          """F."""'],
        )


class TlaOutline(unittest.TestCase):
    def test_definitions_and_their_comments(self) -> None:
        src: str = "---- MODULE M ----\nCONSTANTS\n    N  \\* nodes\n\n\\* The nodes.\nNode == 1..N\n    \\* inside\nNext ==\n    /\\ TRUE\n"
        self.assertEqual(
            outline_tla(src, False),
            ["    1 ---- MODULE M ----", "    2 CONSTANTS", "    3     N  \\* nodes", "    5 \\* The nodes.",
             "    6 Node == 1..N", "    8 Next =="],
        )


if __name__ == "__main__":
    unittest.main()
