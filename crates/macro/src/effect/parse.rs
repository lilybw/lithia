// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! The input of `effect!`: its value or closure and its options (spec §3.1, §3.2).

use super::*;

/// One `name = value` option.
pub(crate) struct OptionArg {
    pub(super) name: Ident,
    pub(super) value: Expr,
}

/// The parsed input of `effect!`: a value or closure, then options.
pub(crate) struct EffectInput {
    pub(super) value: Expr,
    pub(super) options: Vec<OptionArg>,
    pub(super) tokens: String,
}

impl Parse for EffectInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let tokens: String = input.cursor().token_stream().to_string();
        let value: Expr = input.parse()?;
        let mut options: Vec<OptionArg> = Vec::new();
        while !input.is_empty() {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let is_option: bool = input.peek(Ident) && input.peek2(Token![=]) && !input.peek2(Token![==]);
            if !is_option {
                return Err(input.error("expected `name = value`; wrap tuple values in parentheses"));
            }
            let name: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let value: Expr = input.parse()?;
            options.push(OptionArg { name, value });
        }
        Ok(EffectInput { value, options, tokens })
    }
}

/// What `effect!` creates.
pub(super) enum Form<'a> {
    Source(&'a Expr),
    AsyncSource(&'a ExprAsync),
    Derived(&'a ExprClosure),
}

impl EffectInput {
    pub(super) fn form(&self) -> syn::Result<Form<'_>> {
        match &self.value {
            Expr::Closure(closure) if closure.inputs.is_empty() => Err(syn::Error::new_spanned(
                closure,
                "a source node is written `effect!(x)`; to store this closure as the value, \
                 bind it to a name or wrap it in parentheses",
            )),
            Expr::Closure(closure) => Ok(Form::Derived(closure)),
            Expr::Async(block) => Ok(Form::AsyncSource(block)),
            Expr::Assign(assign) => {
                let right: String = assign.right.to_token_stream().to_string();
                Err(syn::Error::new_spanned(
                    assign,
                    format!("an assignment is not a value; write `effect!({right})`, or put options after a comma"),
                ))
            }
            other => Ok(Form::Source(other)),
        }
    }

    /// Records the identifiers a nested `effect!` captures from the enclosing body (spec §4.2).
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
            match (option.name == "key", &option.value) {
                // A key closure's parameters name variables of the enclosing scope.
                (true, Expr::Closure(closure)) => {
                    for input in &closure.inputs {
                        if let Some(ident) = param_ident(input) {
                            captures.read_ident(ident);
                        }
                    }
                    captures.expr(&closure.body);
                }
                _ => captures.expr(&option.value),
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
