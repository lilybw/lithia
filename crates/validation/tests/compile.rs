// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Case AH and the dedicated compile errors of spec §4.6, checked with trybuild.

#[test]
fn compile_errors() {
    let cases: trybuild::TestCases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/*.rs");
    cases.pass("tests/pass/*.rs");
}
