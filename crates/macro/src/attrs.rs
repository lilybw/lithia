//! `#[lithia::main]` and `#[lithia::test]` (spec §3.5).

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{Expr, ItemFn, Lit, Meta, Token};

/// Runtime settings accepted by both attributes, mirroring tokio's.
struct RuntimeArgs {
    current_thread: bool,
    worker_threads: Option<Expr>,
    start_paused: Option<bool>,
}

fn parse_args(args: TokenStream) -> syn::Result<RuntimeArgs> {
    let mut parsed: RuntimeArgs = RuntimeArgs { current_thread: false, worker_threads: None, start_paused: None };
    let metas: Punctuated<Meta, Token![,]> = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(args)?;
    for meta in metas {
        let Meta::NameValue(pair) = &meta else {
            return Err(syn::Error::new_spanned(
                meta,
                "expected `flavor = \"…\"`, `worker_threads = N` or `start_paused = bool`",
            ));
        };
        let name: String = pair.path.get_ident().map(|ident: &syn::Ident| ident.to_string()).unwrap_or_default();
        match name.as_str() {
            "flavor" => match &pair.value {
                Expr::Lit(lit) => match &lit.lit {
                    Lit::Str(text) if text.value() == "multi_thread" => parsed.current_thread = false,
                    Lit::Str(text) if text.value() == "current_thread" => parsed.current_thread = true,
                    _ => {
                        return Err(syn::Error::new_spanned(
                            &pair.value,
                            "flavor is \"multi_thread\" or \"current_thread\"",
                        ));
                    }
                },
                _ => {
                    return Err(syn::Error::new_spanned(
                        &pair.value,
                        "flavor is \"multi_thread\" or \"current_thread\"",
                    ));
                }
            },
            "worker_threads" => parsed.worker_threads = Some(pair.value.clone()),
            "start_paused" => match &pair.value {
                Expr::Lit(lit) => match &lit.lit {
                    Lit::Bool(flag) => parsed.start_paused = Some(flag.value),
                    _ => return Err(syn::Error::new_spanned(&pair.value, "start_paused is a bool")),
                },
                _ => return Err(syn::Error::new_spanned(&pair.value, "start_paused is a bool")),
            },
            _ => {
                return Err(syn::Error::new_spanned(
                    &pair.path,
                    format!("unknown argument `{name}`; expected flavor, worker_threads or start_paused"),
                ));
            }
        }
    }
    if parsed.current_thread && parsed.worker_threads.is_some() {
        return Err(syn::Error::new(proc_macro2::Span::call_site(), "worker_threads needs the multi_thread flavor"));
    }
    Ok(parsed)
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
    if let Some(paused) = args.start_paused {
        tokens.extend(quote!(__lithia_builder.start_paused(#paused);));
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
