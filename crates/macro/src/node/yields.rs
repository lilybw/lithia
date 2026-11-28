// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Generator bodies: finding and lowering `yield`, and whether a body's future completes (spec §4.6).

use super::*;

/// Finds `yield` in a body: direct ones make a generator, nested ones are errors (spec §4.6).
#[derive(Default)]
pub(super) struct YieldScan {
    pub(super) depth: usize,
    pub(super) direct: bool,
    pub(super) nested: Option<Span>,
    /// A `yield` inside a macro call, which the expansion cannot see to lower.
    pub(super) in_macro: Option<Span>,
}

impl<'ast> Visit<'ast> for YieldScan {
    fn visit_expr_yield(&mut self, node: &'ast syn::ExprYield) {
        if self.depth == 0 {
            self.direct = true;
        } else if self.nested.is_none() {
            self.nested = Some(node.yield_token.span);
        }
        syn::visit::visit_expr_yield(self, node);
    }

    fn visit_expr_closure(&mut self, node: &'ast ExprClosure) {
        self.depth += 1;
        syn::visit::visit_expr_closure(self, node);
        self.depth -= 1;
    }

    fn visit_expr_async(&mut self, node: &'ast ExprAsync) {
        self.depth += 1;
        syn::visit::visit_expr_async(self, node);
        self.depth -= 1;
    }

    fn visit_item(&mut self, node: &'ast syn::Item) {
        self.depth += 1;
        syn::visit::visit_item(self, node);
        self.depth -= 1;
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        // A nested `node!`'s yields are its own.
        let nested: bool = node.path.segments.last().is_some_and(|segment: &syn::PathSegment| segment.ident == "node");
        if !nested && self.in_macro.is_none() {
            self.in_macro = yield_in(node.tokens.clone());
        }
    }
}

/// The first `yield` in a macro's tokens, outside any `node!` call in them.
fn yield_in(tokens: TokenStream) -> Option<Span> {
    let trees: Vec<proc_macro2::TokenTree> = tokens.into_iter().collect();
    let mut index: usize = 0;
    while index < trees.len() {
        match &trees[index] {
            proc_macro2::TokenTree::Ident(ident) if ident == "yield" => return Some(ident.span()),
            proc_macro2::TokenTree::Ident(ident) if ident == "node" => {
                let bang: bool =
                    matches!(trees.get(index + 1), Some(proc_macro2::TokenTree::Punct(p)) if p.as_char() == '!');
                if bang {
                    index += 3;
                    continue;
                }
            }
            proc_macro2::TokenTree::Group(group) => {
                if let Some(span) = yield_in(group.stream()) {
                    return Some(span);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// Whether a body yields directly; a `yield` nested in a closure, async block or item is an error.
pub(super) fn scan_yields(visit: impl FnOnce(&mut YieldScan)) -> syn::Result<bool> {
    let mut scan: YieldScan = YieldScan::default();
    visit(&mut scan);
    let mut errors: Option<syn::Error> = None;
    if let Some(span) = scan.nested {
        let message: &str = "`yield` publishes the enclosing node! body's items and only works directly in that body";
        combine(&mut errors, syn::Error::new(span, message));
    }
    if let Some(span) = scan.in_macro {
        let message: &str = "node! cannot see a `yield` inside a macro call such as `select!`; \
                             yield what the macro returns, after it";
        combine(&mut errors, syn::Error::new(span, message));
    }
    match errors {
        Some(errors) => Err(errors),
        None => Ok(scan.direct),
    }
}

/// Rewrites each direct `yield x` into `__lithia_y.item(x).await` (spec §16).
pub(super) struct LowerYield;

impl VisitMut for LowerYield {
    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        if let Expr::Yield(yielded) = expr {
            let span: Span = yielded.yield_token.span;
            let value: TokenStream = match &yielded.expr {
                Some(value) => value.to_token_stream(),
                None => quote!(()),
            };
            let item: Ident = Ident::new("item", span);
            *expr = syn::parse_quote_spanned!(span=> __lithia_y.#item(#value).await);
            return;
        }
        if matches!(expr, Expr::Closure(_) | Expr::Async(_)) {
            return;
        }
        syn::visit_mut::visit_expr_mut(self, expr);
    }

    fn visit_item_mut(&mut self, _item: &mut syn::Item) {}
}

/// Finds what lets a body's future complete: `return`, `?`, or a `break` out of the loop.
#[derive(Default)]
pub(super) struct Exits {
    /// Loops entered inside the loop being checked.
    pub(super) depth: usize,
    /// The checked loop's label.
    pub(super) label: Option<String>,
    pub(super) found: bool,
}

impl<'ast> Visit<'ast> for Exits {
    fn visit_expr_break(&mut self, node: &'ast syn::ExprBreak) {
        let targets: bool = match &node.label {
            Some(label) => Some(label.ident.to_string()) == self.label,
            None => self.depth == 0,
        };
        self.found |= targets;
        syn::visit::visit_expr_break(self, node);
    }

    fn visit_expr_return(&mut self, node: &'ast syn::ExprReturn) {
        self.found = true;
        syn::visit::visit_expr_return(self, node);
    }

    fn visit_expr_try(&mut self, node: &'ast syn::ExprTry) {
        self.found = true;
        syn::visit::visit_expr_try(self, node);
    }

    fn visit_expr_loop(&mut self, node: &'ast syn::ExprLoop) {
        self.depth += 1;
        syn::visit::visit_expr_loop(self, node);
        self.depth -= 1;
    }

    fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
        self.depth += 1;
        syn::visit::visit_expr_while(self, node);
        self.depth -= 1;
    }

    fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
        self.depth += 1;
        syn::visit::visit_expr_for_loop(self, node);
        self.depth -= 1;
    }

    fn visit_expr_closure(&mut self, _: &'ast ExprClosure) {}

    fn visit_expr_async(&mut self, _: &'ast ExprAsync) {}

    fn visit_item(&mut self, _: &'ast syn::Item) {}
}

/// Whether a body ends in a `loop` it can never leave, so its future never completes; its
/// output is then typed `()` rather than left to the never-type fallback.
pub(super) fn never_completes(stmts: &[syn::Stmt]) -> bool {
    let Some(syn::Stmt::Expr(Expr::Loop(tail), None)) = stmts.last() else {
        return false;
    };
    let mut exits: Exits =
        Exits { label: tail.label.as_ref().map(|l: &syn::Label| l.name.ident.to_string()), ..Exits::default() };
    exits.visit_block(&tail.body);
    for stmt in &stmts[..stmts.len() - 1] {
        exits.visit_stmt(stmt);
    }
    !exits.found
}

/// The statements of a body: a block's own, or the expression alone.
pub(super) fn body_stmts(body: &Expr) -> Vec<syn::Stmt> {
    match body {
        Expr::Block(block) if block.attrs.is_empty() && block.label.is_none() => block.block.stmts.clone(),
        other => vec![syn::Stmt::Expr(other.clone(), None)],
    }
}

/// Wraps a body's future so its output is `ty`, when one is known.
pub(super) fn typed_future(ty: Option<TokenStream>, future: TokenStream) -> TokenStream {
    match ty {
        Some(ty) => quote!(::lithia::__private::output::<#ty, _>(#future)),
        None => future,
    }
}
