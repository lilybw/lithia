//! Free-identifier analysis for automatic capture (spec §4.2).

use std::collections::HashSet;

use proc_macro2::{Ident, TokenStream, TokenTree};
use syn::parse::{ParseStream, Parser};
use syn::punctuated::Punctuated;
use syn::{Block, Expr, Macro, Pat, Stmt, Token};

/// Macros whose string literal arguments name captured values inline, as in `"{count}"`.
const FORMAT_MACROS: &[&str] = &[
    "print",
    "println",
    "eprint",
    "eprintln",
    "format",
    "format_args",
    "write",
    "writeln",
    "panic",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "unreachable",
    "todo",
    "unimplemented",
];

/// Collects free, lowercase identifiers in expression position, in first-use order.
#[derive(Default)]
pub(crate) struct Captures {
    scopes: Vec<HashSet<String>>,
    seen: HashSet<String>,
    pub(crate) found: Vec<Ident>,
    pub(crate) uses_self: bool,
}

/// Whether a name is lowercase in the capture rule's sense: not a constant, type or variant.
fn is_lowercase(name: &str) -> bool {
    let name: &str = name.strip_prefix("r#").unwrap_or(name);
    if name == "_" || name == "self" || name == "crate" || name == "super" {
        return false;
    }
    name.chars().next().is_some_and(|first: char| first.is_ascii_lowercase() || first == '_')
}

impl Captures {
    pub(crate) fn new() -> Captures {
        Captures { scopes: vec![HashSet::new()], ..Captures::default() }
    }

    fn is_bound(&self, name: &str) -> bool {
        self.scopes.iter().any(|scope: &HashSet<String>| scope.contains(name))
    }

    fn bind(&mut self, name: &Ident) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string());
        }
    }

    fn push(&mut self) {
        self.scopes.push(HashSet::new());
    }

    fn pop(&mut self) {
        self.scopes.pop();
    }

    /// Records `ident` as captured if it is free and lowercase.
    pub(crate) fn use_ident(&mut self, ident: &Ident) {
        let name: String = ident.to_string();
        if name == "self" {
            self.uses_self = true;
            return;
        }
        if !is_lowercase(&name) || self.is_bound(&name) {
            return;
        }
        if self.seen.insert(name) {
            self.found.push(ident.clone());
        }
    }

    pub(crate) fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Path(path) => {
                if path.qself.is_none() && path.path.leading_colon.is_none() && path.path.segments.len() == 1 {
                    let segment = &path.path.segments[0];
                    if segment.arguments.is_none() {
                        self.use_ident(&segment.ident);
                    }
                }
            }
            Expr::Call(call) => {
                let callee_is_name: bool = matches!(&*call.func, Expr::Path(path)
                    if path.qself.is_none() && path.path.segments.len() == 1 && path.path.leading_colon.is_none());
                if !callee_is_name {
                    self.expr(&call.func);
                }
                for arg in &call.args {
                    self.expr(arg);
                }
            }
            Expr::MethodCall(call) => {
                self.expr(&call.receiver);
                for arg in &call.args {
                    self.expr(arg);
                }
            }
            Expr::Field(field) => self.expr(&field.base),
            Expr::Closure(closure) => {
                self.push();
                for input in &closure.inputs {
                    self.pat(input);
                }
                self.expr(&closure.body);
                self.pop();
            }
            Expr::Block(block) => self.block(&block.block),
            Expr::Async(block) => self.block(&block.block),
            Expr::Unsafe(block) => self.block(&block.block),
            Expr::Const(block) => self.block(&block.block),
            Expr::TryBlock(block) => self.block(&block.block),
            Expr::Let(binding) => {
                self.expr(&binding.expr);
                self.pat(&binding.pat);
            }
            Expr::If(branch) => {
                self.push();
                self.expr(&branch.cond);
                self.block(&branch.then_branch);
                self.pop();
                if let Some((_, otherwise)) = &branch.else_branch {
                    self.expr(otherwise);
                }
            }
            Expr::While(lp) => {
                self.push();
                self.expr(&lp.cond);
                self.block(&lp.body);
                self.pop();
            }
            Expr::ForLoop(lp) => {
                self.expr(&lp.expr);
                self.push();
                self.pat(&lp.pat);
                self.block(&lp.body);
                self.pop();
            }
            Expr::Loop(lp) => self.block(&lp.body),
            Expr::Match(matched) => {
                self.expr(&matched.expr);
                for arm in &matched.arms {
                    self.push();
                    self.pat(&arm.pat);
                    if let Some((_, guard)) = &arm.guard {
                        self.expr(guard);
                    }
                    self.expr(&arm.body);
                    self.pop();
                }
            }
            Expr::Macro(mac) => self.mac(&mac.mac),
            Expr::Struct(lit) => {
                for field in &lit.fields {
                    self.expr(&field.expr);
                }
                if let Some(rest) = &lit.rest {
                    self.expr(rest);
                }
            }
            Expr::Assign(assign) => {
                self.expr(&assign.left);
                self.expr(&assign.right);
            }
            Expr::Binary(binary) => {
                self.expr(&binary.left);
                self.expr(&binary.right);
            }
            Expr::Unary(unary) => self.expr(&unary.expr),
            Expr::Index(index) => {
                self.expr(&index.expr);
                self.expr(&index.index);
            }
            Expr::Range(range) => {
                if let Some(start) = &range.start {
                    self.expr(start);
                }
                if let Some(end) = &range.end {
                    self.expr(end);
                }
            }
            Expr::Reference(reference) => self.expr(&reference.expr),
            Expr::RawAddr(raw) => self.expr(&raw.expr),
            Expr::Paren(paren) => self.expr(&paren.expr),
            Expr::Group(group) => self.expr(&group.expr),
            Expr::Tuple(tuple) => {
                for elem in &tuple.elems {
                    self.expr(elem);
                }
            }
            Expr::Array(array) => {
                for elem in &array.elems {
                    self.expr(elem);
                }
            }
            Expr::Repeat(repeat) => {
                self.expr(&repeat.expr);
                self.expr(&repeat.len);
            }
            Expr::Return(ret) => {
                if let Some(value) = &ret.expr {
                    self.expr(value);
                }
            }
            Expr::Break(brk) => {
                if let Some(value) = &brk.expr {
                    self.expr(value);
                }
            }
            Expr::Yield(yielded) => {
                if let Some(value) = &yielded.expr {
                    self.expr(value);
                }
            }
            Expr::Cast(cast) => self.expr(&cast.expr),
            Expr::Try(attempt) => self.expr(&attempt.expr),
            Expr::Await(awaited) => self.expr(&awaited.base),
            _ => {}
        }
    }

    pub(crate) fn block(&mut self, block: &Block) {
        self.push();
        // Items are visible throughout their block.
        for stmt in &block.stmts {
            if let Stmt::Item(item) = stmt {
                self.item_name(item);
            }
        }
        for stmt in &block.stmts {
            self.stmt(stmt);
        }
        self.pop();
    }

    fn item_name(&mut self, item: &syn::Item) {
        let ident: Option<&Ident> = match item {
            syn::Item::Fn(f) => Some(&f.sig.ident),
            syn::Item::Const(c) => Some(&c.ident),
            syn::Item::Static(s) => Some(&s.ident),
            _ => None,
        };
        if let Some(ident) = ident {
            self.bind(ident);
        }
    }

    pub(crate) fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Local(local) => {
                if let Some(init) = &local.init {
                    self.expr(&init.expr);
                    if let Some((_, diverge)) = &init.diverge {
                        self.expr(diverge);
                    }
                }
                self.pat(&local.pat);
            }
            Stmt::Item(_) => {}
            Stmt::Expr(expr, _) => self.expr(expr),
            Stmt::Macro(mac) => self.mac(&mac.mac),
        }
    }

    /// Binds every name a pattern introduces.
    pub(crate) fn pat(&mut self, pat: &Pat) {
        match pat {
            Pat::Ident(binding) => {
                self.bind(&binding.ident);
                if let Some((_, sub)) = &binding.subpat {
                    self.pat(sub);
                }
            }
            Pat::Tuple(tuple) => {
                for elem in &tuple.elems {
                    self.pat(elem);
                }
            }
            Pat::TupleStruct(tuple) => {
                for elem in &tuple.elems {
                    self.pat(elem);
                }
            }
            Pat::Struct(record) => {
                for field in &record.fields {
                    self.pat(&field.pat);
                }
            }
            Pat::Slice(slice) => {
                for elem in &slice.elems {
                    self.pat(elem);
                }
            }
            Pat::Or(or) => {
                for case in &or.cases {
                    self.pat(case);
                }
            }
            Pat::Reference(reference) => self.pat(&reference.pat),
            Pat::Type(typed) => self.pat(&typed.pat),
            Pat::Paren(paren) => self.pat(&paren.pat),
            _ => {}
        }
    }

    fn mac(&mut self, mac: &Macro) {
        let name: String = mac.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
        if name == "effect"
            && let Ok(nested) = syn::parse2::<crate::effect::EffectInput>(mac.tokens.clone())
        {
            nested.visit_captures(self);
            return;
        }
        if FORMAT_MACROS.contains(&name.as_str())
            && let Ok(args) = Punctuated::<Expr, Token![,]>::parse_terminated.parse2(mac.tokens.clone())
        {
            {
                let mut named: HashSet<String> = HashSet::new();
                for arg in &args {
                    if let Expr::Assign(assign) = arg
                        && let Expr::Path(path) = &*assign.left
                        && let Some(ident) = path.path.get_ident()
                    {
                        named.insert(ident.to_string());
                        self.expr(&assign.right);
                        continue;
                    }
                    self.expr(arg);
                }
                for arg in &args {
                    if let Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(text), .. }) = arg {
                        for name in inline_args(&text.value()) {
                            if !named.contains(&name) {
                                self.use_ident(&Ident::new(&name, text.span()));
                            }
                        }
                    }
                }
                return;
            }
        }
        // Other macros: best effort.
        if let Ok(args) = Punctuated::<Expr, Token![,]>::parse_terminated.parse2(mac.tokens.clone()) {
            for arg in &args {
                self.expr(arg);
            }
            return;
        }
        let repeat = |input: ParseStream<'_>| -> syn::Result<(Expr, Expr)> {
            let value: Expr = input.parse()?;
            input.parse::<Token![;]>()?;
            let count: Expr = input.parse()?;
            Ok((value, count))
        };
        if let Ok((value, count)) = repeat.parse2(mac.tokens.clone()) {
            self.expr(&value);
            self.expr(&count);
            return;
        }
        if let Ok(stmts) = Block::parse_within.parse2(mac.tokens.clone()) {
            self.push();
            for stmt in &stmts {
                self.stmt(stmt);
            }
            self.pop();
        }
    }
}

/// Names used inline in a format string: `{name}` and `{name:…}`.
pub(crate) fn inline_args(text: &str) -> Vec<String> {
    let bytes: &[u8] = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i: usize = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' if bytes.get(i + 1) == Some(&b'{') => i += 2,
            b'}' if bytes.get(i + 1) == Some(&b'}') => i += 2,
            b'{' => {
                let Some(close) = text[i + 1..].find('}') else {
                    break;
                };
                let inner: &str = &text[i + 1..i + 1 + close];
                let (name, spec): (&str, &str) = inner.split_once(':').unwrap_or((inner, ""));
                push_name(&mut out, name.trim());
                // Width and precision may name arguments too: `{:>width$}`, `{:.prec$}`.
                for (end, _) in spec.match_indices('$') {
                    let start: usize = spec[..end]
                        .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                        .map_or(0, |at: usize| at + 1);
                    push_name(&mut out, &spec[start..end]);
                }
                i += close + 2;
            }
            _ => i += 1,
        }
    }
    out
}

fn push_name(out: &mut Vec<String>, name: &str) {
    let valid: bool = name.chars().next().is_some_and(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c: char| c.is_ascii_alphanumeric() || c == '_');
    if valid && is_lowercase(name) && !out.iter().any(|existing: &String| existing == name) {
        out.push(name.to_string());
    }
}

/// Replaces `self` with `__self` in a token stream, leaving `self::` paths alone.
pub(crate) fn rewrite_self(tokens: TokenStream) -> TokenStream {
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    let mut out: Vec<TokenTree> = Vec::with_capacity(trees.len());
    for (index, tree) in trees.iter().enumerate() {
        match tree {
            TokenTree::Ident(ident) if ident == "self" => {
                let path_follows: bool = matches!(
                    (trees.get(index + 1), trees.get(index + 2)),
                    (Some(TokenTree::Punct(a)), Some(TokenTree::Punct(b))) if a.as_char() == ':' && b.as_char() == ':'
                );
                if path_follows {
                    out.push(tree.clone());
                } else {
                    out.push(TokenTree::Ident(Ident::new("__self", ident.span())));
                }
            }
            TokenTree::Group(group) => {
                let mut rebuilt: proc_macro2::Group =
                    proc_macro2::Group::new(group.delimiter(), rewrite_self(group.stream()));
                rebuilt.set_span(group.span());
                out.push(TokenTree::Group(rebuilt));
            }
            other => out.push(other.clone()),
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured(src: &str) -> Vec<String> {
        let expr: Expr = syn::parse_str(src).expect("parses");
        let mut captures: Captures = Captures::new();
        captures.expr(&expr);
        captures.found.iter().map(|i: &Ident| i.to_string()).collect()
    }

    #[test]
    fn collects_free_lowercase_names() {
        assert_eq!(captured("a.get() + b * MAX"), vec!["a", "b"]);
        assert_eq!(captured("{ let x = y; x + z }"), vec!["y", "z"]);
        assert_eq!(captured("max(a, b)"), vec!["a", "b"]);
        assert_eq!(captured("|n: &u32| n + k"), vec!["k"]);
        assert_eq!(captured("for i in 1..=*count.get() { yield i; }"), vec!["count"]);
        assert_eq!(captured("match v { Some(x) => x, None => d }"), vec!["v", "d"]);
        assert_eq!(captured("Point { x, y: w }"), vec!["x", "w"]);
    }

    #[test]
    fn scans_format_strings() {
        assert_eq!(captured("println!(\"{count} {other:?} {0} {{lit}}\", x)"), vec!["x", "count", "other"]);
        assert_eq!(captured("format!(\"{v}\", v = w)"), vec!["w"]);
    }

    #[test]
    fn sees_into_nested_effects() {
        assert_eq!(captured("Some(effect!(|n| n.update(|v: &u32| v + 1)))"), vec!["n"]);
        assert_eq!(captured("effect!(async |id| { cache.set(id.get()); })"), vec!["id", "cache"]);
    }

    #[test]
    fn inline_arg_parsing() {
        assert_eq!(inline_args("{a} {b:>4} {} {0} {{c}} {Upper}"), vec!["a", "b"]);
        assert_eq!(inline_args("{:>width$} {v:.prec$} {0:1$}"), vec!["width", "v", "prec"]);
    }
}
