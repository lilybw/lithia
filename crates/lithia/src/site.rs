// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Where a node was created (spec §4.3).

use std::fmt;

/// A site's identity within one build: the address of its static `Site`.
pub(crate) type SiteKey = usize;

/// Where a node was created, recorded at compile time for diagnostics.
/// `eprintln!("{}:{} in {}", site.file, site.line, (site.function)());`
#[derive(Clone, Copy)]
#[non_exhaustive]
pub struct Site {
    /// Source file of the `effect!` call.
    pub file: &'static str,
    /// Line of the `effect!` call.
    pub line: u32,
    /// Column of the `effect!` call.
    pub column: u32,
    /// Module path of the `effect!` call.
    pub module: &'static str,
    /// Path of the enclosing function, resolved on first call.
    pub function: fn() -> &'static str,
    /// The start of the macro input, stringified; empty in builds without debug assertions.
    pub tokens: &'static str,
    /// Nodes from this site that support holds, for the pile-up warning (spec §12).
    pub(crate) held: &'static SiteCounter,
}

/// A count kept per site, beside its `Site`, so counting takes no shared lock.
#[doc(hidden)]
pub struct SiteCounter(
    // A diagnostic count, never synchronising anything, so loom need not see it.
    #[allow(clippy::disallowed_types)] pub(crate) std::sync::atomic::AtomicUsize,
);

impl SiteCounter {
    #[allow(clippy::disallowed_types)]
    pub const fn new() -> SiteCounter {
        SiteCounter(std::sync::atomic::AtomicUsize::new(0))
    }
}

impl Default for SiteCounter {
    fn default() -> SiteCounter {
        SiteCounter::new()
    }
}

impl Site {
    /// Builds a site; only `effect!` does, so a field can be added without breaking callers.
    #[doc(hidden)]
    pub const fn __new(
        file: &'static str,
        line: u32,
        column: u32,
        module: &'static str,
        function: fn() -> &'static str,
        tokens: &'static str,
        held: &'static SiteCounter,
    ) -> Site {
        Site { file, line, column, module, function, tokens, held }
    }
}

impl fmt::Debug for Site {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Site")
            .field("file", &self.file)
            .field("line", &self.line)
            .field("column", &self.column)
            .field("module", &self.module)
            .field("function", &(self.function)())
            .field("tokens", &self.tokens)
            .finish()
    }
}

impl fmt::Display for Site {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{} in {}", self.file, self.line, self.column, (self.function)())
    }
}

/// Name of the marker function `effect!` declares to find its enclosing function.
pub(crate) const SCOPE_MARKER: &str = "__lithia_scope";

/// Trims the marker function's own name off a `type_name`, leaving the enclosing path.
/// `assert_eq!(fn_path("app::run::__lithia_scope"), "app::run");`
pub fn fn_path(type_name: &'static str) -> &'static str {
    let trimmed: &'static str = type_name.strip_suffix(SCOPE_MARKER).unwrap_or(type_name);
    trimmed.strip_suffix("::").unwrap_or(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_marker() {
        assert_eq!(fn_path("a::b::__lithia_scope"), "a::b");
        assert_eq!(fn_path("a::b::{{closure}}::__lithia_scope"), "a::b::{{closure}}");
        assert_eq!(fn_path("plain"), "plain");
    }
}
