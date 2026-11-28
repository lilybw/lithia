// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! The input of `node!`: its value or closure and its options (spec §3.1, §3.2).

use super::*;

/// One `name = value` option, or a bare `name` flag.
pub(crate) struct OptionArg {
    pub(super) name: Ident,
    pub(super) value: Option<Expr>,
}

/// The parsed input of `node!`: a value or closure, then options.
pub(crate) struct NodeInput {
    pub(super) value: Expr,
    pub(super) options: Vec<OptionArg>,
    pub(super) tokens: String,
}

impl Parse for NodeInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        reserved(input.cursor().token_stream())?;
        let tokens: String = input.cursor().token_stream().to_string();
        let value: Expr = input.parse()?;
        let mut options: Vec<OptionArg> = Vec::new();
        while !input.is_empty() {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let is_option: bool = input.peek(Ident) && input.peek2(Token![=]) && !input.peek2(Token![==]);
            if is_option {
                let name: Ident = input.parse()?;
                input.parse::<Token![=]>()?;
                let value: Expr = input.parse()?;
                options.push(OptionArg { name, value: Some(value) });
                continue;
            }
            let ahead: ParseStream<'_> = &input.fork();
            let is_flag: bool = ahead.parse::<Ident>().is_ok() && (ahead.is_empty() || ahead.peek(Token![,]));
            if !is_flag {
                return Err(input.error("expected `name = value` or `no_equals`; wrap tuple values in parentheses"));
            }
            options.push(OptionArg { name: input.parse()?, value: None });
        }
        Ok(NodeInput { value, options, tokens })
    }
}

/// Attributes on a body's closure or block, which the expansion does not carry over.
const DROPPED_ATTRIBUTE: &str =
    "node! does not carry attributes on its closure or block over; put them on the enclosing statement";

/// Adds `error` to those found so far, so every error in the input is reported at once.
pub(super) fn combine(errors: &mut Option<syn::Error>, error: syn::Error) {
    match errors {
        Some(errors) => errors.combine(error),
        None => *errors = Some(error),
    }
}

/// An expression without the invisible group a `macro_rules!` fragment arrives in.
pub(super) fn ungrouped(expr: &Expr) -> &Expr {
    match expr {
        Expr::Group(group) => ungrouped(&group.expr),
        other => other,
    }
}

/// What `node!` creates.
pub(super) enum Form<'a> {
    Source(&'a Expr),
    AsyncSource(&'a ExprAsync),
    Derived(&'a ExprClosure),
}

impl NodeInput {
    pub(super) fn form(&self) -> syn::Result<Form<'_>> {
        match &self.value {
            Expr::Closure(closure) if closure.inputs.is_empty() => Err(syn::Error::new_spanned(
                closure,
                "a source node is written `node!(x)`; to store this closure as the value, \
                 bind it to a name or wrap it in parentheses",
            )),
            Expr::Closure(closure) => {
                if let Some(attr) = closure.attrs.first() {
                    return Err(syn::Error::new_spanned(attr, DROPPED_ATTRIBUTE));
                }
                if closure.asyncness.is_none()
                    && let Expr::Async(block) = ungrouped(&closure.body)
                {
                    return Err(syn::Error::new_spanned(
                        block.async_token,
                        "a closure that returns an async block makes a node whose value is a future; \
                         write `async |…| { … }` or `async move |…| { … }` for an async body",
                    ));
                }
                Ok(Form::Derived(closure))
            }
            Expr::Async(block) => match block.attrs.first() {
                Some(attr) => Err(syn::Error::new_spanned(attr, DROPPED_ATTRIBUTE)),
                None => Ok(Form::AsyncSource(block)),
            },
            Expr::Assign(assign) => {
                let right: String = assign.right.to_token_stream().to_string();
                Err(syn::Error::new_spanned(
                    assign,
                    format!("an assignment is not a value; write `node!({right})`, or put options after a comma"),
                ))
            }
            other => Ok(Form::Source(other)),
        }
    }

    /// Records the identifiers a nested `node!` captures from the enclosing body (spec §4.2).
    pub(crate) fn visit_captures(&self, captures: &mut Captures) {
        match &self.value {
            Expr::Closure(closure) => {
                for input in &closure.inputs {
                    if let Some(ident) = param_ident(input) {
                        captures.read_ident(ident);
                    }
                }
                captures.expr(&closure.body);
            }
            other => captures.expr(other),
        }
        for option in &self.options {
            let Some(value) = &option.value else { continue };
            match (option.name == "key", value) {
                // A key closure's parameters name variables of the enclosing scope.
                (true, Expr::Closure(closure)) => {
                    for input in &closure.inputs {
                        if let Some(ident) = param_ident(input) {
                            captures.read_ident(ident);
                        }
                    }
                    captures.expr(&closure.body);
                }
                _ => captures.expr(value),
            }
        }
    }
}

/// The identifier of a plain parameter, `name` or `name: Type`.
pub(super) fn param_ident(pat: &Pat) -> Option<&Ident> {
    match pat {
        Pat::Ident(binding) if binding.by_ref.is_none() && binding.mutability.is_none() && binding.subpat.is_none() => {
            Some(&binding.ident)
        }
        Pat::Type(typed) => param_ident(&typed.pat),
        _ => None,
    }
}

pub(super) fn param_type(pat: &Pat) -> Option<&Type> {
    match pat {
        Pat::Type(typed) => Some(&typed.ty),
        _ => None,
    }
}

/// Rejects the names the expansion keeps for itself, so a body cannot reach its internals.
fn reserved(tokens: TokenStream) -> syn::Result<()> {
    for tree in tokens {
        match tree {
            proc_macro2::TokenTree::Ident(ident) if is_internal(&ident) => {
                return Err(syn::Error::new(ident.span(), "names starting with `__lithia` are node!'s own"));
            }
            proc_macro2::TokenTree::Group(group) => reserved(group.stream())?,
            _ => {}
        }
    }
    Ok(())
}

/// Whether `ident` is one of the expansion's own names.
pub(super) fn is_internal(ident: &Ident) -> bool {
    let name: String = ident.to_string();
    let name: &str = name.strip_prefix("r#").unwrap_or(&name);
    name.starts_with("__lithia") || name.starts_with("__LITHIA")
}
