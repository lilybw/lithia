//! Where a node was created (spec §4.3).

use std::fmt;

/// Where a node was created, recorded at compile time for diagnostics.
/// `eprintln!("{}:{} in {}", site.file, site.line, (site.function)());`
#[derive(Clone, Copy)]
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
    /// The macro input as written.
    pub tokens: &'static str,
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
pub const SCOPE_MARKER: &str = "__lithia_scope";

/// Trims the marker function's own name off a `type_name`, leaving the enclosing path.
/// `assert_eq!(fn_path("app::run::__lithia_scope"), "app::run");`
pub fn fn_path(type_name: &'static str) -> &'static str {
    let trimmed: &'static str = type_name.strip_suffix(SCOPE_MARKER).unwrap_or(type_name);
    trimmed.strip_suffix("::").unwrap_or(trimmed)
}

/// A site for nodes created without `effect!`, such as in lithia's own tests.
pub(crate) static UNKNOWN_SITE: Site = Site {
    file: "<unknown>",
    line: 0,
    column: 0,
    module: "<unknown>",
    function: unknown_function,
    tokens: "",
};

fn unknown_function() -> &'static str {
    "<unknown>"
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
