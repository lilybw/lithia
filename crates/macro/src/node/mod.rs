// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! Parsing and expansion of `node!` (spec §3.1, §3.2, §4).

use std::collections::HashSet;

use proc_macro2::{Span, TokenStream};
use quote::{ToTokens, format_ident, quote, quote_spanned};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::visit_mut::VisitMut;
use syn::{Expr, ExprAsync, ExprClosure, Ident, Pat, Token, Type};

use crate::captures::{Captures, Class, FieldPath, RewriteSelf};

mod emit;
mod keyed;
mod options;
mod parse;
mod yields;

pub(crate) use parse::NodeInput;

use emit::*;
use keyed::*;
use options::*;
use parse::*;
use yields::*;

pub(crate) fn expand(input: NodeInput) -> syn::Result<TokenStream> {
    let form: Form<'_> = input.form()?;
    let resolved: Resolved = resolve_options(&input, &form)?;
    let site: TokenStream = site(&input.tokens);
    let chains: Chains = Chains::of(&resolved);
    let Chains { bind, create: options, same: same_options }: &Chains = &chains;
    // Without `init`, `K::default()`, or a compile error naming `init` when `K` has none (spec §4.4).
    let init: TokenStream = match &resolved.init {
        Some(init) => init.to_token_stream(),
        None => quote!(<_ as ::lithia::__private::InitDefault>::__lithia_init()),
    };
    let key: Option<&KeyArg> = resolved.key.as_ref();
    let expansion: TokenStream = match (form, key) {
        (Form::Source(value), None) => quote! {{
            #site
            ::lithia::__private::source(&__LITHIA_SITE, #value, #options)
        }},
        (Form::Source(value), Some(key)) => {
            let key_value: TokenStream = key.value();
            let key_ref: TokenStream = key.lookup();
            let body: TokenStream = lookup_or_create(
                quote!(::lithia::__private::reclaim_source(#key_ref, &__lithia_shape, &__lithia_probe)),
                quote!(source),
                same_options,
                quote!(::lithia::__private::source(&__LITHIA_SITE, __lithia_value, #options)),
            );
            quote! {{
                #site
                let __lithia_key = #key_value;
                #bind
                let __lithia_value = #value;
                let __lithia_probe = {
                    #[allow(unused_imports)]
                    use ::lithia::__private::{ProbeViaArc as _, ProbeViaEq as _, ProbeViaHandle as _, ProbeViaOpaque as _};
                    (&&&&::lithia::__private::Probed(&__lithia_value)).__lithia_probe()
                };
                let __lithia_probes: [&dyn ::lithia::__private::Probe; 0] = [];
                #body
            }}
        }
        (Form::AsyncSource(block), key) => expand_async_source(block, &site, &init, &chains, key)?,
        (Form::Derived(closure), key) => expand_derived(closure, &site, &init, &chains, key)?,
    };
    Ok(hygienic(expansion))
}

/// Gives the expansion's own names mixed-site hygiene where they stand: the user's code, which
/// cannot write them, cannot reach them either, and every use the expansion makes still does.
fn hygienic(tokens: TokenStream) -> TokenStream {
    tokens
        .into_iter()
        .map(|tree: proc_macro2::TokenTree| match tree {
            proc_macro2::TokenTree::Ident(mut ident) if is_internal(&ident) => {
                ident.set_span(Span::mixed_site().located_at(ident.span()));
                proc_macro2::TokenTree::Ident(ident)
            }
            proc_macro2::TokenTree::Group(group) => {
                let mut rebuilt: proc_macro2::Group =
                    proc_macro2::Group::new(group.delimiter(), hygienic(group.stream()));
                rebuilt.set_span(group.span());
                proc_macro2::TokenTree::Group(rebuilt)
            }
            other => other,
        })
        .collect()
}

fn expand_async_source(
    block: &ExprAsync,
    site: &TokenStream,
    init: &TokenStream,
    chains: &Chains,
    key: Option<&KeyArg>,
) -> syn::Result<TokenStream> {
    let options: &TokenStream = &chains.create;
    let generator: bool = scan_yields(|scan: &mut YieldScan| scan.visit_block(&block.block))?;
    let moved: bool = block.capture.is_some();
    let mut body: syn::Block = block.block.clone();
    if generator {
        LowerYield.visit_block_mut(&mut body);
    }
    let Analysis { names, classes, self_class }: Analysis = analyse_block(&block.block);
    let probed: TokenStream = probes(&names, self_class.is_some());
    let self_class: Option<Class> = self_class.filter(|_: &Class| !moved);
    let uses_self: bool = self_class.is_some();
    let takes: Vec<TokenStream> = names
        .iter()
        .zip(&classes)
        .map(|(ident, class): (&Ident, &Class)| capture_take(ident, class, moved, false))
        .collect();
    let self_capture: TokenStream = self_class.as_ref().map(self_clone).unwrap_or_default();
    // The block's statements are spliced after the rebinding, so a one-expression block does
    // not trip `unused_braces`.
    if uses_self {
        RewriteSelf.visit_block_mut(&mut body);
    }
    let stmts: &[syn::Stmt] = &body.stmts;
    let body_tokens: TokenStream = quote!(#(#stmts)*);
    // A block without `move` sees its clones by reference, as every body does; a `move` block runs
    // once and owns what it moved in, so it can consume it.
    let mut shared: Vec<Ident> = if moved { Vec::new() } else { names };
    if uses_self {
        shared.push(format_ident!("__self"));
    }
    let rebind: TokenStream = by_reference(&shared);
    let begin: TokenStream = session_begin();
    let end: TokenStream = session_end();
    let call: TokenStream = if generator {
        quote! {
            ::lithia::__private::source_gen(
                &__LITHIA_SITE,
                &__lithia_captured,
                #init,
                move |__lithia_y: ::lithia::__private::Yielder<_>| async move { #rebind #body_tokens },
                #options,
            )
        }
    } else {
        let ty: Option<TokenStream> = never_completes(&block.block.stmts).then(|| quote!(()));
        let future: TokenStream = typed_future(ty, quote!(async move { #rebind #body_tokens }));
        quote! {
            ::lithia::__private::source_async(
                &__LITHIA_SITE,
                &__lithia_captured,
                #init,
                #future,
                #options,
            )
        }
    };
    let create: TokenStream = quote! {
        #begin
        #(#takes)*
        #self_capture
        #end
        #call
    };
    let Some(key) = key else {
        return Ok(quote! {{
            #site
            #create
        }});
    };
    let key_value: TokenStream = key.value();
    let key_ref: TokenStream = key.lookup();
    let body: TokenStream = lookup_or_create(
        quote!(::lithia::__private::reclaim_source_async(#key_ref, &__lithia_shape, &__lithia_probes)),
        quote!(source),
        &chains.same,
        create,
    );
    let bind: &TokenStream = &chains.bind;
    Ok(quote! {{
        #site
        let __lithia_key = #key_value;
        #bind
        #probed
        #body
    }})
}

fn expand_derived(
    closure: &ExprClosure,
    site: &TokenStream,
    init: &TokenStream,
    chains: &Chains,
    key: Option<&KeyArg>,
) -> syn::Result<TokenStream> {
    let options: &TokenStream = &chains.create;
    // Dependencies: plain identifiers, each once (spec §4.1).
    let mut params: Vec<Ident> = Vec::new();
    let mut checks: Vec<TokenStream> = Vec::new();
    let mut names: HashSet<String> = HashSet::new();
    let mut errors: Option<syn::Error> = None;
    for input in &closure.inputs {
        let named: Option<&Ident> = param_ident(input).filter(|ident: &&Ident| names.insert(ident.to_string()));
        let Some(ident) = named else {
            combine(
                &mut errors,
                syn::Error::new_spanned(input, "dependencies are named by plain identifiers, each once"),
            );
            continue;
        };
        if let Some(ty) = param_type(input) {
            checks.push(quote_spanned!(ty.span()=> let _: &#ty = &#ident;));
        }
        params.push(ident.clone());
    }
    if let Some(errors) = errors {
        return Err(errors);
    }
    let count: usize = params.len();
    // Each handle is taken at its parameter, so an error that it cannot be read points there.
    let handles: Vec<TokenStream> = params
        .iter()
        .map(|param: &Ident| quote_spanned!(param.span()=> ::lithia::__private::node_ref(&#param)))
        .collect();
    let deps: TokenStream = quote! {
        let __lithia_deps: [::lithia::__private::NodeHandle; #count] = [#(#handles),*];
    };

    let is_async: bool = closure.asyncness.is_some();
    let moved: bool = closure.capture.is_some();
    let generator: bool = scan_yields(|scan: &mut YieldScan| scan.visit_expr(&closure.body))?;
    let mut body: Expr = (*closure.body).clone();
    if generator {
        LowerYield.visit_expr_mut(&mut body);
    }

    let Analysis { names: captured, classes, self_class }: Analysis = analyse(&closure.body);
    let uses_self: bool = self_class.is_some();
    let probed: TokenStream = probes(&captured, uses_self);
    // A keyed node is looked up first, so a reclaimed one with equal captures clones nothing.
    let wrap = |create: TokenStream| -> TokenStream {
        let Some(key) = key else {
            return quote! {{
                #site
                #deps
                #(#checks)*
                #create
            }};
        };
        let key_value: TokenStream = key.value();
        let key_ref: TokenStream = key.lookup();
        let body: TokenStream = lookup_or_create(
            quote!(::lithia::__private::reclaim_derived(#key_ref, &__lithia_shape, &__lithia_deps, &__lithia_probes)),
            quote!(derived),
            &chains.same,
            create,
        );
        let bind: &TokenStream = &chains.bind;
        quote! {{
            #site
            #deps
            #(#checks)*
            let __lithia_key = #key_value;
            #bind
            #probed
            #body
        }}
    };
    let takes: Vec<TokenStream> = captured
        .iter()
        .zip(&classes)
        .map(|(ident, class): (&Ident, &Class)| {
            let dependency: bool = params.iter().any(|param: &Ident| param == ident);
            capture_take(ident, class, moved, dependency)
        })
        .collect();
    let begin: TokenStream = session_begin();
    let end: TokenStream = session_end();
    if uses_self && !moved {
        RewriteSelf.visit_expr_mut(&mut body);
    }
    let mut body_tokens: TokenStream = body.to_token_stream();
    {
        // Spliced after the rebinding, so a one-expression block does not trip `unused_braces`.
        if let Expr::Block(block) = &body
            && block.attrs.is_empty()
            && block.label.is_none()
        {
            let stmts: &[syn::Stmt] = &block.block.stmts;
            body_tokens = quote!(#(#stmts)*);
        }
    }
    let self_capture: TokenStream = match (&self_class, moved) {
        (Some(class), false) => self_clone(class),
        _ => TokenStream::new(),
    };

    if !is_async && !generator {
        // Synchronous body: captures live in the closure, which lives in one Arc; each run sees
        // them by reference, as every other body form does.
        let mut shared: Vec<Ident> = captured;
        if uses_self && !moved {
            shared.push(format_ident!("__self"));
        }
        let rebind: TokenStream = by_reference(&shared);
        let closure_tokens: TokenStream = match &closure.output {
            syn::ReturnType::Type(arrow, ty) => quote!(move || #arrow #ty { #rebind #body_tokens }),
            syn::ReturnType::Default => quote!(move || { #rebind #body_tokens }),
        };
        return Ok(wrap(quote! {
            #begin
            #(#takes)*
            #self_capture
            #end
            ::lithia::__private::derived(&__LITHIA_SITE, &__lithia_deps, &__lithia_captured, #init, #closure_tokens, #options)
        }));
    }

    // Async bodies and generators: captures live in one Arc, cloned once per run.
    let mut bundle: Vec<Ident> = captured;
    if uses_self && !moved {
        bundle.push(format_ident!("__self"));
    }
    let unpacked: TokenStream = unpack(&bundle);
    let call: TokenStream = if generator {
        // A generator's `-> T` is the type of its items.
        let item: TokenStream = match &closure.output {
            syn::ReturnType::Type(_, ty) => ty.to_token_stream(),
            syn::ReturnType::Default => quote!(_),
        };
        quote! {
            ::lithia::__private::derived_gen(
                &__LITHIA_SITE,
                &__lithia_deps,
                &__lithia_captured,
                #init,
                move |__lithia_y: ::lithia::__private::Yielder<#item>| {
                    let __lithia_caps = ::std::sync::Arc::clone(&__lithia_caps);
                    async move {
                        #unpacked
                        #body_tokens
                    }
                },
                #options,
            )
        }
    } else {
        let ty: Option<TokenStream> = match &closure.output {
            syn::ReturnType::Type(_, ty) => Some(ty.to_token_stream()),
            syn::ReturnType::Default => never_completes(&body_stmts(&closure.body)).then(|| quote!(())),
        };
        let future: TokenStream = typed_future(ty, quote!(async move { #unpacked #body_tokens }));
        quote! {
            ::lithia::__private::derived_async(
                &__LITHIA_SITE,
                &__lithia_deps,
                &__lithia_captured,
                #init,
                move || {
                    let __lithia_caps = ::std::sync::Arc::clone(&__lithia_caps);
                    #future
                },
                #options,
            )
        }
    };
    Ok(wrap(quote! {
        #begin
        #(#takes)*
        #self_capture
        #end
        let __lithia_caps = ::std::sync::Arc::new((#(#bundle,)*));
        #call
    }))
}
