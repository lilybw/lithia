// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! `#[lithia::main]` and `#[lithia::test]` (spec §3.5).

use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{Expr, ItemFn, Lit, Meta, Token};

/// Runtime settings accepted by both attributes, mirroring tokio's.
struct RuntimeArgs {
    current_thread: bool,
    worker_threads: Option<Expr>,
    /// The value, and the argument's name to point an error at.
    start_paused: Option<(bool, syn::Path)>,
}

/// Expands an attribute; on an error, emits the function unchanged beside it, so the function
/// still exists for the IDE and for the rest of the crate.
pub(crate) fn with_item(
    expand: fn(TokenStream, TokenStream) -> syn::Result<TokenStream>,
    args: TokenStream,
    item: TokenStream,
) -> TokenStream {
    match expand(args, item.clone()) {
        Ok(tokens) => tokens,
        Err(error) => {
            let mut tokens: TokenStream = error.to_compile_error();
            tokens.extend(item);
            tokens
        }
    }
}

const FLAVOR: &str = "flavor is \"multi_thread\" or \"current_thread\"";

fn parse_args(args: TokenStream) -> syn::Result<RuntimeArgs> {
    let mut parsed: RuntimeArgs = RuntimeArgs { current_thread: false, worker_threads: None, start_paused: None };
    let metas: Punctuated<Meta, Token![,]> = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(args)?;
    let mut errors: Option<syn::Error> = None;
    for meta in metas {
        let Meta::NameValue(pair) = &meta else {
            combine(
                &mut errors,
                syn::Error::new_spanned(
                    meta,
                    "expected `flavor = \"…\"`, `worker_threads = N` or `start_paused = bool`",
                ),
            );
            continue;
        };
        let name: String = pair.path.get_ident().map(|ident: &syn::Ident| ident.to_string()).unwrap_or_default();
        match name.as_str() {
            "flavor" => match &pair.value {
                Expr::Lit(lit) => match &lit.lit {
                    Lit::Str(text) if text.value() == "multi_thread" => parsed.current_thread = false,
                    Lit::Str(text) if text.value() == "current_thread" => parsed.current_thread = true,
                    _ => combine(&mut errors, syn::Error::new_spanned(&pair.value, FLAVOR)),
                },
                _ => combine(&mut errors, syn::Error::new_spanned(&pair.value, FLAVOR)),
            },
            "worker_threads" => {
                if let Expr::Lit(lit) = &pair.value
                    && let Lit::Int(count) = &lit.lit
                    && count.base10_parse::<u64>().is_ok_and(|count: u64| count == 0)
                {
                    combine(&mut errors, syn::Error::new_spanned(&pair.value, "worker_threads is at least 1"));
                    continue;
                }
                parsed.worker_threads = Some(pair.value.clone());
            }
            "start_paused" => match &pair.value {
                Expr::Lit(lit) => match &lit.lit {
                    Lit::Bool(flag) => parsed.start_paused = Some((flag.value, pair.path.clone())),
                    _ => combine(&mut errors, syn::Error::new_spanned(&pair.value, "start_paused is a bool")),
                },
                _ => combine(&mut errors, syn::Error::new_spanned(&pair.value, "start_paused is a bool")),
            },
            _ => combine(
                &mut errors,
                syn::Error::new_spanned(
                    &pair.path,
                    format!("unknown argument `{name}`; expected flavor, worker_threads or start_paused"),
                ),
            ),
        }
    }
    if parsed.current_thread
        && let Some(threads) = &parsed.worker_threads
    {
        combine(&mut errors, syn::Error::new_spanned(threads, "worker_threads needs the multi_thread flavor"));
    }
    match errors {
        Some(errors) => Err(errors),
        None => Ok(parsed),
    }
}

fn builder(args: &RuntimeArgs) -> TokenStream {
    let mut tokens: TokenStream = if args.current_thread {
        quote!(let mut __lithia_builder: ::lithia::__private::tokio::runtime::Builder =
            ::lithia::__private::tokio::runtime::Builder::new_current_thread();)
    } else {
        quote!(let mut __lithia_builder: ::lithia::__private::tokio::runtime::Builder =
            ::lithia::__private::tokio::runtime::Builder::new_multi_thread();)
    };
    if let Some(threads) = &args.worker_threads {
        tokens.extend(quote!(__lithia_builder.worker_threads(#threads);));
    }
    // Through a trait that names lithia's `test-util` feature when tokio's clock control is off.
    if let Some((paused, at)) = &args.start_paused {
        tokens.extend(quote_spanned! {at.span()=>
            <() as ::lithia::__private::StartPaused>::start_paused(&mut __lithia_builder, #paused);
        });
    }
    tokens
}

fn async_fn(item: TokenStream, attribute: &str) -> syn::Result<ItemFn> {
    let function: ItemFn = syn::parse2(item)?;
    if function.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            function.sig.fn_token,
            format!("#[lithia::{attribute}] needs an `async fn`; the async keyword is missing"),
        ));
    }
    Ok(function)
}

pub(crate) fn main(args: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let runtime: RuntimeArgs = parse_args(args)?;
    let mut function: ItemFn = async_fn(item, "main")?;
    function.sig.asyncness = None;
    let builder: TokenStream = builder(&runtime);
    let body: &syn::Block = &function.block;
    let attrs: &[syn::Attribute] = &function.attrs;
    let vis: &syn::Visibility = &function.vis;
    let sig: &syn::Signature = &function.sig;
    Ok(quote! {
        #(#attrs)*
        #vis #sig {
            #builder
            ::lithia::__private::block_on_main(__lithia_builder, async move #body)
        }
    })
}

pub(crate) fn test(args: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let runtime: RuntimeArgs = parse_args(args)?;
    let mut function: ItemFn = async_fn(item, "test")?;
    function.sig.asyncness = None;
    let builder: TokenStream = builder(&runtime);
    let name: String = function.sig.ident.to_string();
    let body: &syn::Block = &function.block;
    let attrs: &[syn::Attribute] = &function.attrs;
    let vis: &syn::Visibility = &function.vis;
    let sig: &syn::Signature = &function.sig;
    Ok(quote! {
        #[::core::prelude::v1::test]
        #(#attrs)*
        #vis #sig {
            #builder
            ::lithia::__private::block_on_test(__lithia_builder, #name, async move #body)
        }
    })
}

/// Adds `error` to those found so far, so every error in the arguments is reported at once.
fn combine(errors: &mut Option<syn::Error>, error: syn::Error) {
    match errors {
        Some(errors) => errors.combine(error),
        None => *errors = Some(error),
    }
}
