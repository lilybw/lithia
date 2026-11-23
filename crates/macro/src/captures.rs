// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Free-identifier analysis for automatic capture (spec §4.2).

use std::collections::{HashMap, HashSet};

use proc_macro2::{Ident, Spacing, TokenStream, TokenTree};
use syn::parse::{ParseStream, Parser};
use syn::punctuated::Punctuated;
use syn::{Block, Expr, Macro, Pat, Stmt, Token};

/// Macros that format their arguments, by the position of their format string; `{count}` inside
/// it names a captured value inline, and their arguments are only read.
const FORMAT_MACROS: &[(&str, usize)] = &[
    ("print", 0),
    ("println", 0),
    ("eprint", 0),
    ("eprintln", 0),
    ("format", 0),
    ("format_args", 0),
    ("panic", 0),
    ("unreachable", 0),
    ("todo", 0),
    ("unimplemented", 0),
    ("anyhow", 0),
    ("bail", 0),
    ("eyre", 0),
    ("write", 1),
    ("writeln", 1),
    ("assert", 1),
    ("debug_assert", 1),
    ("ensure", 1),
    ("assert_eq", 2),
    ("assert_ne", 2),
    ("debug_assert_eq", 2),
    ("debug_assert_ne", 2),
];

/// `tracing`'s (and `log`'s) event and span macros, whose fields take `%` and `?` sigils.
const TRACING_MACROS: &[&str] = &[
    "trace",
    "debug",
    "info",
    "warn",
    "error",
    "event",
    "span",
    "trace_span",
    "debug_span",
    "info_span",
    "warn_span",
    "error_span",
];

/// Keywords a token walk never takes for a name.
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn",
    "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self",
    "static", "struct", "super", "trait", "true", "try", "type", "unsafe", "use", "where", "while", "yield",
];

/// The named fields that follow a captured name, as in `state.rows.selected`.
pub(crate) type FieldPath = Vec<Ident>;
/// The names bound in one block scope.
type Bindings = HashSet<String>;

/// Collects free, lowercase identifiers in expression position, in first-use order.
#[derive(Default)]
pub(crate) struct Captures {
    scopes: Vec<Bindings>,
    seen: HashSet<String>,
    pub(crate) found: Vec<Ident>,
    pub(crate) uses_self: bool,
    /// How each captured name, `self` included, is used.
    uses: HashMap<String, Uses>,
    /// Depth of macros the analysis cannot see into; every use inside one may write.
    opaque: u32,
}

/// The uses of one captured name.
#[derive(Default)]
struct Uses {
    /// Used other than through a field path.
    direct: bool,
    /// Used directly other than to read it.
    written: bool,
    /// Field paths used, each with whether a use of it may write.
    paths: Vec<(FieldPath, bool)>,
}

/// How a capture's handles are classified for writer edges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// A handle the body only reads.
    ReadOnly,
    /// Everything recorded inside it may be written.
    Write,
    /// Used only through field paths; each path that may write, by its fields.
    Paths { all: Vec<FieldPath>, written: Vec<FieldPath> },
}

/// Macros whose arguments are plain expressions, evaluated as written.
const TRANSPARENT_MACROS: &[&str] = &["vec", "dbg"];

/// Methods that only read a handle.
const READ_METHODS: &[&str] = &["get", "try_get", "changes", "accessor"];

/// Whether a name is lowercase in the capture rule's sense: not a constant, type or variant. Leading
/// underscores are skipped, so `_LIMIT` is a constant and `_count` a variable.
fn is_lowercase(name: &str) -> bool {
    let name: &str = name.strip_prefix("r#").unwrap_or(name);
    if name == "_" || name == "self" || name == "crate" || name == "super" {
        return false;
    }
    match name.trim_start_matches('_').chars().next() {
        Some(first) => first.is_lowercase() || first.is_ascii_digit(),
        None => true,
    }
}

impl Captures {
    pub(crate) fn new() -> Captures {
        Captures { scopes: vec![HashSet::new()], ..Captures::default() }
    }

    fn is_bound(&self, name: &str) -> bool {
        self.scopes.iter().any(|scope: &Bindings| scope.contains(name))
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

    /// Records `ident` as captured if it is free and lowercase; the use may write it.
    pub(crate) fn use_ident(&mut self, ident: &Ident) {
        if let Some(uses) = self.capture(ident) {
            uses.direct = true;
            uses.written = true;
        }
    }

    /// Records `ident` as captured, used only to read it.
    pub(crate) fn read_ident(&mut self, ident: &Ident) {
        if self.opaque > 0 {
            return self.use_ident(ident);
        }
        if let Some(uses) = self.capture(ident) {
            uses.direct = true;
        }
    }

    /// Records a use of the field path `base.fields…`; `read` when the use only reads it.
    fn path_use(&mut self, base: &Ident, fields: FieldPath, read: bool) {
        let written: bool = !read || self.opaque > 0;
        if let Some(uses) = self.capture(base) {
            match uses.paths.iter_mut().find(|(known, _): &&mut (FieldPath, bool)| *known == fields) {
                Some((_, was)) => *was |= written,
                None => uses.paths.push((fields, written)),
            }
        }
    }

    /// How a captured name (or `self`) is classified.
    pub(crate) fn class(&self, name: &str) -> Class {
        let Some(uses) = self.uses.get(name) else {
            return Class::ReadOnly;
        };
        if uses.paths.is_empty() {
            return if uses.written { Class::Write } else { Class::ReadOnly };
        }
        if uses.direct {
            return Class::Write;
        }
        Class::Paths {
            all: uses.paths.iter().map(|(fields, _): &(FieldPath, bool)| fields.clone()).collect(),
            written: uses
                .paths
                .iter()
                .filter(|(_, written): &&(FieldPath, bool)| *written)
                .map(|(fields, _): &(FieldPath, bool)| fields.clone())
                .collect(),
        }
    }

    fn capture(&mut self, ident: &Ident) -> Option<&mut Uses> {
        let name: String = ident.to_string();
        if name == "self" {
            self.uses_self = true;
            return Some(self.uses.entry(name).or_default());
        }
        if !is_lowercase(&name) || self.is_bound(&name) {
            return None;
        }
        if self.seen.insert(name.clone()) {
            self.found.push(ident.clone());
        }
        Some(self.uses.entry(name).or_default())
    }

    /// A field path: a captured name, or `self`, followed by named fields only.
    fn field_path(expr: &Expr) -> Option<(&Ident, FieldPath)> {
        match expr {
            Expr::Field(field) => {
                let syn::Member::Named(member) = &field.member else {
                    return None;
                };
                let (base, mut fields): (&Ident, FieldPath) = match &*field.base {
                    Expr::Field(_) => Captures::field_path(&field.base)?,
                    other => (Captures::bare_name(other)?, Vec::new()),
                };
                fields.push(member.clone());
                Some((base, fields))
            }
            Expr::Paren(paren) => Captures::field_path(&paren.expr),
            _ => None,
        }
    }

    /// The single name an expression is, if it is a bare path.
    fn bare_name(expr: &Expr) -> Option<&Ident> {
        match expr {
            Expr::Path(path)
                if path.qself.is_none() && path.path.leading_colon.is_none() && path.path.segments.len() == 1 =>
            {
                let segment: &syn::PathSegment = &path.path.segments[0];
                segment.arguments.is_none().then_some(&segment.ident)
            }
            Expr::Paren(paren) => Captures::bare_name(&paren.expr),
            _ => None,
        }
    }

    /// Visits `expr` as a read: a bare name or field path, or a read method call on one, counts as a read.
    fn read_expr(&mut self, expr: &Expr) {
        if let Some(ident) = Captures::bare_name(expr) {
            return self.read_ident(ident);
        }
        if let Some((base, fields)) = Captures::field_path(expr) {
            return self.path_use(base, fields, true);
        }
        self.expr(expr)
    }

    pub(crate) fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Path(path) => {
                if path.qself.is_none() && path.path.leading_colon.is_none() && path.path.segments.len() == 1 {
                    let segment: &syn::PathSegment = &path.path.segments[0];
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
                // `lithia::status(&name)` only reads its argument.
                let status: bool = matches!(&*call.func, Expr::Path(path)
                    if path.path.segments.last().is_some_and(|s: &syn::PathSegment| s.ident == "status")
                        && path.path.segments.len() == 2
                        && path.path.segments[0].ident == "lithia");
                for arg in &call.args {
                    match arg {
                        Expr::Reference(reference) if status && reference.mutability.is_none() => {
                            self.read_expr(&reference.expr)
                        }
                        _ => self.expr(arg),
                    }
                }
            }
            Expr::MethodCall(call) => {
                let reads: bool = call.args.is_empty() && READ_METHODS.contains(&call.method.to_string().as_str());
                if reads {
                    self.read_expr(&call.receiver);
                } else {
                    self.expr(&call.receiver);
                }
                for arg in &call.args {
                    self.expr(arg);
                }
            }
            Expr::Field(field) => match Captures::field_path(expr) {
                Some((base, fields)) => self.path_use(base, fields, false),
                None => self.expr(&field.base),
            },
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
                    self.expr(&arm.body);
                    self.pop();
                }
            }
            Expr::Macro(mac) => self.mac(&mac.mac, false),
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
            syn::Item::Use(used) => return self.use_tree(&used.tree, None),
            _ => None,
        };
        if let Some(ident) = ident {
            self.bind(ident);
        }
    }

    /// Binds the names a `use` item brings in; `last` is the path segment `self` would name.
    fn use_tree(&mut self, tree: &syn::UseTree, last: Option<&Ident>) {
        match tree {
            syn::UseTree::Path(path) => self.use_tree(&path.tree, Some(&path.ident)),
            syn::UseTree::Name(name) if name.ident == "self" => {
                if let Some(last) = last {
                    self.bind(last);
                }
            }
            syn::UseTree::Name(name) => self.bind(&name.ident),
            syn::UseTree::Rename(rename) => self.bind(&rename.rename),
            syn::UseTree::Group(group) => {
                for tree in &group.items {
                    self.use_tree(tree, last);
                }
            }
            syn::UseTree::Glob(_) => {}
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
            Stmt::Macro(mac) => self.mac(&mac.mac, true),
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
            // A match guard sees the bindings of the pattern it follows.
            Pat::Guard(guarded) => {
                self.pat(&guarded.pat);
                self.expr(&guarded.guard);
            }
            _ => {}
        }
    }

    /// Visits a macro call; `statement` when it stands as a statement, whose bindings then reach the
    /// statements after it, as `tokio::pin!`'s do.
    fn mac(&mut self, mac: &Macro, statement: bool) {
        let name: String =
            mac.path.segments.last().map(|segment: &syn::PathSegment| segment.ident.to_string()).unwrap_or_default();
        let known: bool = match name.as_str() {
            "effect" => match syn::parse2::<crate::effect::EffectInput>(mac.tokens.clone()) {
                Ok(nested) => {
                    nested.visit_captures(self);
                    true
                }
                Err(_) => false,
            },
            "matches" => self.matches_mac(mac),
            "select" => self.select_mac(mac),
            _ if TRACING_MACROS.contains(&name.as_str()) => self.tracing_mac(mac),
            _ => match FORMAT_MACROS.iter().find(|(known, _): &&(&str, usize)| *known == name) {
                Some((_, at)) => self.format_mac(mac, Some(*at)),
                None => false,
            },
        };
        if known {
            return;
        }
        if TRANSPARENT_MACROS.contains(&name.as_str()) {
            return self.opaque_mac(mac, statement);
        }
        // Other macros: best effort, and every use inside may write.
        self.opaque += 1;
        self.opaque_mac(mac, statement);
        self.opaque -= 1;
    }

    /// Arguments that are expressions, only read, with the format string at `at` (or, when `None`,
    /// the first string literal) scanned for inline names. False if they do not parse.
    fn format_mac(&mut self, mac: &Macro, at: Option<usize>) -> bool {
        let Ok(args) = Punctuated::<Expr, Token![,]>::parse_terminated.parse2(mac.tokens.clone()) else {
            return false;
        };
        let mut named: HashSet<String> = HashSet::new();
        for arg in &args {
            if let Expr::Assign(assign) = arg
                && let Expr::Path(path) = &*assign.left
                && let Some(ident) = path.path.get_ident()
            {
                named.insert(ident.to_string());
                self.read_expr(&assign.right);
                continue;
            }
            self.read_expr(arg);
        }
        self.scan_format(&args, at, &named);
        true
    }

    /// Reads the names the format string at `at`, or the first string literal, uses inline.
    fn scan_format(&mut self, args: &Punctuated<Expr, Token![,]>, at: Option<usize>, named: &HashSet<String>) {
        let literal: Option<&syn::LitStr> = match at {
            Some(at) => args.iter().nth(at).and_then(string_literal),
            None => args.iter().find_map(string_literal),
        };
        if let Some(text) = literal {
            for name in inline_args(&text.value()) {
                if !named.contains(&name) {
                    self.read_ident(&Ident::new(&name, text.span()));
                }
            }
        }
    }

    /// `matches!(value, pattern if guard)`: the guard sees the pattern's bindings.
    fn matches_mac(&mut self, mac: &Macro) -> bool {
        let parser = |input: ParseStream<'_>| -> syn::Result<(Expr, Pat, Option<Expr>)> {
            let value: Expr = input.parse()?;
            input.parse::<Token![,]>()?;
            let pat: Pat = Pat::parse_multi_with_leading_vert(input)?;
            let guard: Option<Expr> = if input.peek(Token![if]) {
                input.parse::<Token![if]>()?;
                Some(input.parse()?)
            } else {
                None
            };
            input.parse::<Option<Token![,]>>()?;
            Ok((value, pat, guard))
        };
        let Ok((value, pat, guard)) = parser.parse2(mac.tokens.clone()) else {
            return false;
        };
        self.expr(&value);
        self.push();
        self.pat(&pat);
        if let Some(guard) = &guard {
            self.expr(guard);
        }
        self.pop();
        true
    }

    /// `tokio::select!` and `futures::select!`: `pattern = future [, if condition] => handler`, with
    /// `else`, `default` or `complete` arms; each handler sees its pattern's bindings.
    fn select_mac(&mut self, mac: &Macro) -> bool {
        let parser = |input: ParseStream<'_>| -> syn::Result<Vec<SelectArm>> {
            let mut arms: Vec<SelectArm> = Vec::new();
            if input.peek(syn::Ident) && input.peek2(Token![;]) {
                input.parse::<Ident>()?;
                input.parse::<Token![;]>()?;
            }
            while !input.is_empty() {
                let bare: bool = input.peek(Token![else])
                    || input.peek(Token![default])
                    || (input.peek(syn::Ident) && input.peek2(Token![=>]));
                let (pat, future, condition): (Option<Pat>, Option<Expr>, Option<Expr>) = if bare {
                    if input.peek(Token![else]) {
                        input.parse::<Token![else]>()?;
                    } else if input.peek(Token![default]) {
                        input.parse::<Token![default]>()?;
                    } else {
                        input.parse::<Ident>()?;
                    }
                    (None, None, None)
                } else {
                    let pat: Pat = Pat::parse_single(input)?;
                    input.parse::<Token![=]>()?;
                    let future: Expr = input.parse()?;
                    let condition: Option<Expr> = if input.peek(Token![,]) && input.peek2(Token![if]) {
                        input.parse::<Token![,]>()?;
                        input.parse::<Token![if]>()?;
                        Some(input.parse()?)
                    } else {
                        None
                    };
                    (Some(pat), Some(future), condition)
                };
                input.parse::<Token![=>]>()?;
                let handler: Expr = input.parse()?;
                input.parse::<Option<Token![,]>>()?;
                arms.push(SelectArm { pat, future, condition, handler });
            }
            Ok(arms)
        };
        let Ok(arms) = parser.parse2(mac.tokens.clone()) else {
            return false;
        };
        for arm in &arms {
            if let Some(future) = &arm.future {
                self.expr(future);
            }
            if let Some(condition) = &arm.condition {
                self.expr(condition);
            }
            self.push();
            if let Some(pat) = &arm.pat {
                self.pat(pat);
            }
            self.expr(&arm.handler);
            self.pop();
        }
        true
    }

    /// `tracing`'s fields: `name = %value`, `?value`, `target: …`, then a format string and its
    /// arguments; everything is only read. False if a part does not parse.
    fn tracing_mac(&mut self, mac: &Macro) -> bool {
        let mut message: bool = false;
        for part in split_commas(mac.tokens.clone()) {
            let trees: Vec<TokenTree> = part.into_iter().collect();
            if trees.is_empty() {
                continue;
            }
            // `target: …`, `parent: …` and `name: …` take an expression.
            if let [TokenTree::Ident(_), TokenTree::Punct(colon), rest @ ..] = &trees[..]
                && colon.as_char() == ':'
                && colon.spacing() == Spacing::Alone
                && !message
            {
                let Ok(value) = syn::parse2::<Expr>(rest.iter().cloned().collect()) else {
                    return false;
                };
                self.read_expr(&value);
                continue;
            }
            if !message
                && let [TokenTree::Literal(_)] = &trees[..]
                && let Ok(text) = syn::parse2::<syn::LitStr>(trees.iter().cloned().collect())
            {
                message = true;
                for name in inline_args(&text.value()) {
                    self.read_ident(&Ident::new(&name, text.span()));
                }
                continue;
            }
            // A field or named argument: the name before `=` names nothing in the body.
            let start: usize = field_value_start(&trees);
            let mut value: &[TokenTree] = &trees[start..];
            if let [TokenTree::Punct(sigil), rest @ ..] = value
                && matches!(sigil.as_char(), '%' | '?')
            {
                value = rest;
            }
            let Ok(value) = syn::parse2::<Expr>(value.iter().cloned().collect()) else {
                return false;
            };
            self.read_expr(&value);
        }
        true
    }

    fn opaque_mac(&mut self, mac: &Macro, statement: bool) {
        if let Ok(args) = Punctuated::<Expr, Token![,]>::parse_terminated.parse2(mac.tokens.clone()) {
            for arg in &args {
                self.expr(arg);
            }
            self.scan_format(&args, None, &HashSet::new());
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
            if !statement {
                self.push();
            }
            for stmt in &stmts {
                self.stmt(stmt);
            }
            if !statement {
                self.pop();
            }
            return;
        }
        // Unparsable: every name that could be a variable is taken for one the macro may write.
        self.opaque += 1;
        self.token_walk(mac.tokens.clone());
        self.opaque -= 1;
    }

    /// Takes every free lowercase name in `tokens` that is not a field, method, path segment, macro
    /// name, label, keyword or binding's type annotation for a use that may write.
    fn token_walk(&mut self, tokens: TokenStream) {
        let trees: Vec<TokenTree> = tokens.into_iter().collect();
        for (index, tree) in trees.iter().enumerate() {
            match tree {
                TokenTree::Group(group) => self.token_walk(group.stream()),
                TokenTree::Ident(ident) => {
                    let before: Option<char> = index.checked_sub(1).and_then(|at: usize| punct(&trees[at]));
                    let after: Option<&TokenTree> = trees.get(index + 1);
                    let named_after: bool = matches!(after, Some(TokenTree::Punct(p)) if p.as_char() == '!'
                        || (p.as_char() == ':' && p.spacing() == Spacing::Joint)
                        || (p.as_char() == ':' && p.spacing() == Spacing::Alone));
                    let skip: bool = matches!(before, Some('.' | ':' | '\''))
                        || named_after
                        || KEYWORDS.contains(&ident.to_string().as_str());
                    if !skip {
                        self.use_ident(ident);
                    }
                }
                _ => {}
            }
        }
    }
}

/// One arm of a `select!`.
struct SelectArm {
    pat: Option<Pat>,
    future: Option<Expr>,
    condition: Option<Expr>,
    handler: Expr,
}

fn string_literal(expr: &Expr) -> Option<&syn::LitStr> {
    match expr {
        Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(text), .. }) => Some(text),
        _ => None,
    }
}

fn punct(tree: &TokenTree) -> Option<char> {
    match tree {
        TokenTree::Punct(p) => Some(p.as_char()),
        _ => None,
    }
}

/// A token stream split at its top-level commas.
fn split_commas(tokens: TokenStream) -> Vec<TokenStream> {
    let mut parts: Vec<TokenStream> = vec![TokenStream::new()];
    for tree in tokens {
        match &tree {
            TokenTree::Punct(p) if p.as_char() == ',' => parts.push(TokenStream::new()),
            _ => {
                if let Some(last) = parts.last_mut() {
                    last.extend(std::iter::once(tree));
                }
            }
        }
    }
    parts
}

/// Where a field's value starts: after its top-level `=` (not `==`, `=>`, `<=`, `>=` or `!=`), else 0.
fn field_value_start(trees: &[TokenTree]) -> usize {
    for (index, tree) in trees.iter().enumerate() {
        if let TokenTree::Punct(p) = tree
            && p.as_char() == '='
            && p.spacing() == Spacing::Alone
        {
            let joined: bool = index
                .checked_sub(1)
                .and_then(|at: usize| match &trees[at] {
                    TokenTree::Punct(prev) if prev.spacing() == Spacing::Joint => Some(prev.as_char()),
                    _ => None,
                })
                .is_some();
            if !joined {
                return index + 1;
            }
        }
    }
    0
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
    use proc_macro2::Span;

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
    fn sees_match_guards() {
        assert_eq!(captured("match x { Some(v) if v > limit.get() => v, w => w }"), vec!["x", "limit"]);
        assert_eq!(classes("match 1 { _ if a.set(1) => 0, _ => 1 }"), vec![("a".into(), Class::Write)]);
    }

    #[test]
    fn sees_keys_of_nested_effects() {
        // A key closure's parameters name variables of the enclosing body, read by reference.
        assert_eq!(
            captured("effect!(|tick| *tick.get(), key = |row, user| (row.id, user.0))"),
            vec!["tick", "row", "user"]
        );
        assert_eq!(captured("effect!(title.clone(), key = row.id)"), vec!["title", "row"]);
    }

    fn classes(src: &str) -> Vec<(String, Class)> {
        let expr: Expr = syn::parse_str(src).expect("parses");
        let mut captures: Captures = Captures::new();
        captures.expr(&expr);
        let mut names: Vec<String> = captures.found.iter().map(|i: &Ident| i.to_string()).collect();
        if captures.uses_self {
            names.push("self".to_string());
        }
        names.into_iter().map(|name: String| (name.clone(), captures.class(&name))).collect()
    }

    fn fields(paths: &[&[&str]]) -> Vec<FieldPath> {
        paths
            .iter()
            .map(|path: &&[&str]| path.iter().map(|f: &&str| Ident::new(f, Span::call_site())).collect())
            .collect()
    }

    #[test]
    fn classifies_reads_and_writes() {
        let read: Class = Class::ReadOnly;
        let write: Class = Class::Write;
        assert_eq!(
            classes("(a.get(), b.try_get(), c.changes(), d.accessor())"),
            vec![
                ("a".into(), read.clone()),
                ("b".into(), read.clone()),
                ("c".into(), read.clone()),
                ("d".into(), read.clone())
            ]
        );
        assert_eq!(classes("lithia::status(&a)"), vec![("a".into(), read.clone())]);
        assert_eq!(
            classes("println!(\"{a} {}\", b.get())"),
            vec![("b".into(), read.clone()), ("a".into(), read.clone())]
        );
        assert_eq!(classes("println!(\"{:?}\", a.set(1))"), vec![("a".into(), write.clone())]);
        assert_eq!(classes("helper(&a)"), vec![("a".into(), write.clone())]);
        assert_eq!(classes("items.get(0).unwrap().set(5)"), vec![("items".into(), write.clone())]);
        assert_eq!(classes("my_macro!(a.get())"), vec![("a".into(), write)]);
        assert_eq!(classes("vec![a.get()]"), vec![("a".into(), read.clone())]);
        assert_eq!(classes("effect!(|a| b.get())"), vec![("a".into(), read.clone()), ("b".into(), read)]);
    }

    #[test]
    fn classifies_field_paths() {
        assert_eq!(
            classes("{ self.total.set(1); self.theme.get() }"),
            vec![(
                "self".into(),
                Class::Paths { all: fields(&[&["total"], &["theme"]]), written: fields(&[&["total"]]) }
            )]
        );
        assert_eq!(
            classes("app.user.name.get()"),
            vec![("app".into(), Class::Paths { all: fields(&[&["user", "name"]]), written: Vec::new() })]
        );
        assert_eq!(classes("{ self.theme.get(); self.notify(1) }"), vec![("self".into(), Class::Write)]);
        assert_eq!(
            classes("pair.items[0].get()"),
            vec![("pair".into(), Class::Paths { all: fields(&[&["items"]]), written: fields(&[&["items"]]) })]
        );
        assert_eq!(classes("pair.0.get()"), vec![("pair".into(), Class::Write)]);
    }

    #[test]
    fn underscored_constants_and_used_names() {
        assert_eq!(captured("_LIMIT + _count + __CAP"), vec!["_count"]);
        assert_eq!(captured("ä + Ö"), vec!["ä"]);
        assert_eq!(
            captured("{ use std::f64::consts::PI as pi; use a::{b, c as d, e::{self}}; pi * r + b + d + e }"),
            vec!["r"]
        );
    }

    #[test]
    fn statement_macros_bind_for_later_statements() {
        assert_eq!(captured("{ tokio::pin! { let fut = source(a); } fut.poll() }"), vec!["a"]);
        assert_eq!(captured("{ let x = m!({ let y = 1; y }); y }"), vec!["y"]);
    }

    #[test]
    fn scans_only_the_format_string() {
        assert_eq!(captured("assert_eq!(x, \"{name}\")"), vec!["x"]);
        assert_eq!(captured("assert_eq!(a, b, \"{c} {}\", d)"), vec!["a", "b", "d", "c"]);
        assert_eq!(captured("write!(out, \"{v}\")"), vec!["out", "v"]);
        assert_eq!(captured("anyhow!(\"{err}\")"), vec!["err"]);
        assert_eq!(captured("my_log!(level, \"{v}\")"), vec!["level", "v"]);
    }

    #[test]
    fn sees_into_matches_select_and_tracing() {
        assert_eq!(
            classes("matches!(x, Some(v) if { t.set(v); true })"),
            vec![("x".into(), Class::Write), ("t".into(), Class::Write)]
        );
        assert_eq!(captured("tokio::select! { biased; v = rx.recv() => out.set(v), else => {} }"), vec!["rx", "out"]);
        assert_eq!(
            captured("tokio::select! { Some(v) = rx.recv(), if open.get() => { v } _ = sleep(d) => 0 }"),
            vec!["rx", "open", "d"]
        );
        assert_eq!(
            classes("tracing::info!(target: \"t\", c = %target.get(), ?other, \"{count:?} {}\", n.get())"),
            vec![
                ("target".into(), Class::ReadOnly),
                ("other".into(), Class::ReadOnly),
                ("count".into(), Class::ReadOnly),
                ("n".into(), Class::ReadOnly)
            ]
        );
    }

    #[test]
    fn unparsable_macros_capture_every_name_as_written() {
        assert_eq!(
            classes("weird!(@ a.b => c::D, 'l: { e } f: G)"),
            vec![("a".into(), Class::Write), ("e".into(), Class::Write)]
        );
    }

    #[test]
    fn inline_arg_parsing() {
        assert_eq!(inline_args("{a} {b:>4} {} {0} {{c}} {Upper}"), vec!["a", "b"]);
        assert_eq!(inline_args("{:>width$} {v:.prec$} {0:1$}"), vec!["width", "v", "prec"]);
    }
}
