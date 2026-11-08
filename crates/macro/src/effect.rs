//! Parsing and expansion of `effect!` (spec §3.1, §3.2, §4).

use std::collections::HashSet;

use proc_macro2::{Span, TokenStream};
use quote::{ToTokens, format_ident, quote, quote_spanned};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::visit_mut::VisitMut;
use syn::{Expr, ExprAsync, ExprClosure, Ident, Pat, Token, Type};

use crate::captures::{Captures, rewrite_self};

const OPTIONS: &[&str] = &["init", "equals", "fragility", "ghost", "flags"];

/// One `name = value` option.
pub(crate) struct OptionArg {
    name: Ident,
    value: Expr,
}

/// The parsed input of `effect!`: a value or closure, then options.
pub(crate) struct EffectInput {
    value: Expr,
    options: Vec<OptionArg>,
    tokens: String,
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
enum Form<'a> {
    Source(&'a Expr),
    AsyncSource(&'a ExprAsync),
    Derived(&'a ExprClosure),
}

impl EffectInput {
    fn form(&self) -> syn::Result<Form<'_>> {
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
                        captures.use_ident(ident);
                    }
                }
                captures.expr(&closure.body);
            }
            other => captures.expr(other),
        }
        for option in &self.options {
            captures.expr(&option.value);
        }
    }
}

/// The identifier of a plain parameter, `name` or `name: Type`.
fn param_ident(pat: &Pat) -> Option<&Ident> {
    match pat {
        Pat::Ident(binding) if binding.by_ref.is_none() && binding.mutability.is_none() && binding.subpat.is_none() => {
            Some(&binding.ident)
        }
        Pat::Type(typed) => param_ident(&typed.pat),
        _ => None,
    }
}

fn param_type(pat: &Pat) -> Option<&Type> {
    match pat {
        Pat::Type(typed) => Some(&typed.ty),
        _ => None,
    }
}

/// Finds `yield` in a body: direct ones make a generator, nested ones are errors (spec §4.6).
#[derive(Default)]
struct YieldScan {
    depth: usize,
    direct: bool,
    nested: Option<Span>,
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
}

/// Whether a body yields directly; a `yield` nested in a closure, async block or item is an error.
fn scan_yields(visit: impl FnOnce(&mut YieldScan)) -> syn::Result<bool> {
    let mut scan: YieldScan = YieldScan::default();
    visit(&mut scan);
    if let Some(span) = scan.nested {
        return Err(syn::Error::new(
            span,
            "`yield` publishes the enclosing effect! body's items and only works directly in that body",
        ));
    }
    Ok(scan.direct)
}

/// Rewrites each direct `yield x` into `__lithia_y.item(x).await` (spec §16).
struct LowerYield;

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

/// Validated options, as token streams ready for the expansion.
struct Resolved {
    init: Option<Expr>,
    chain: TokenStream,
}

fn resolve_options(input: &EffectInput, form: &Form<'_>) -> syn::Result<Resolved> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut init: Option<Expr> = None;
    let mut chain: TokenStream = quote!(::lithia::__private::Options::new());
    let mut has_equals: bool = false;
    let mut always_propagate: Option<Span> = None;
    for option in &input.options {
        let name: String = option.name.to_string();
        if !OPTIONS.contains(&name.as_str()) {
            return Err(syn::Error::new_spanned(
                &option.name,
                format!("unknown option `{name}`; expected one of init, equals, fragility, ghost, flags"),
            ));
        }
        if !seen.insert(name.clone()) {
            return Err(syn::Error::new_spanned(&option.name, format!("option `{name}` is given twice")));
        }
        let value: &Expr = &option.value;
        let consts: TokenStream = quote!(
            #[allow(unused_imports)]
            use ::lithia::__private::consts::*;
        );
        match name.as_str() {
            "init" => {
                if let Form::Source(_) = form {
                    return Err(syn::Error::new_spanned(
                        &option.name,
                        "`init` applies to derived nodes and async source nodes; a source node starts with its value",
                    ));
                }
                init = Some(value.clone());
            }
            "equals" => {
                has_equals = true;
                chain = quote!(#chain.equals(#value));
            }
            "fragility" => {
                if !matches!(form, Form::Derived(_)) {
                    return Err(syn::Error::new_spanned(
                        &option.name,
                        "`fragility` applies to derived nodes only; a source node has no parents",
                    ));
                }
                chain = quote!(#chain.fragility({ #consts #value }));
            }
            "ghost" => {
                chain = quote!(#chain.ghost({ #consts #value }));
            }
            "flags" => {
                if value
                    .to_token_stream()
                    .into_iter()
                    .any(|token: proc_macro2::TokenTree| token.to_string() == "ALWAYS_PROPAGATE")
                {
                    always_propagate = Some(option.name.span());
                }
                chain = quote!(#chain.flags({ #consts #value }));
            }
            _ => unreachable!(),
        }
    }
    if let (Some(span), false) = (always_propagate, has_equals) {
        return Err(syn::Error::new(span, "`ALWAYS_PROPAGATE` without `equals` does nothing; add `equals = …`"));
    }
    Ok(Resolved { init, chain })
}

fn site(tokens: &str) -> TokenStream {
    quote! {
        fn __lithia_scope() {}
        fn __lithia_path() -> &'static str {
            ::lithia::__private::fn_path(::core::any::type_name_of_val(&__lithia_scope))
        }
        static __LITHIA_SITE: ::lithia::Site = ::lithia::Site {
            file: ::core::file!(),
            line: ::core::line!(),
            column: ::core::column!(),
            module: ::core::module_path!(),
            function: __lithia_path,
            tokens: #tokens,
        };
    }
}

fn capture_clone(ident: &Ident) -> TokenStream {
    quote_spanned! {ident.span()=>
        #[allow(unused_variables)]
        let #ident = {
            #[allow(unused_imports)]
            use ::lithia::__private::CaptureAny as _;
            (#ident).__lithia_capture()
        };
    }
}

fn self_clone() -> TokenStream {
    quote! {
        #[allow(unused_variables)]
        let __self = {
            #[allow(unused_imports)]
            use ::lithia::__private::CaptureAny as _;
            (self).__lithia_capture()
        };
    }
}

/// Identifiers a body captures, plus whether it uses `self`.
fn analyse(body: &Expr) -> (Vec<Ident>, bool) {
    let mut captures: Captures = Captures::new();
    captures.expr(body);
    (captures.found, captures.uses_self)
}

fn analyse_block(block: &syn::Block) -> (Vec<Ident>, bool) {
    let mut captures: Captures = Captures::new();
    captures.block(block);
    (captures.found, captures.uses_self)
}

/// Binds every capture by reference from the shared `Arc<Captures>` inside a run's future.
fn unpack(names: &[Ident]) -> TokenStream {
    if names.is_empty() {
        return quote!(let _ = &__lithia_caps;);
    }
    quote! {
        #[allow(unused_variables)]
        let (#(#names,)*) = &*__lithia_caps;
    }
}

pub(crate) fn expand(input: EffectInput) -> syn::Result<TokenStream> {
    let form: Form<'_> = input.form()?;
    let resolved: Resolved = resolve_options(&input, &form)?;
    let site: TokenStream = site(&input.tokens);
    let options: &TokenStream = &resolved.chain;
    let init: TokenStream = match &resolved.init {
        Some(init) => init.to_token_stream(),
        None => quote!(<_ as ::lithia::__private::InitDefault>::__lithia_init()),
    };
    let expansion: TokenStream = match form {
        Form::Source(value) => quote! {{
            #site
            ::lithia::__private::source(&__LITHIA_SITE, #value, #options)
        }},
        Form::AsyncSource(block) => expand_async_source(block, &site, &init, options)?,
        Form::Derived(closure) => expand_derived(closure, &site, &init, options)?,
    };
    Ok(expansion)
}

fn expand_async_source(
    block: &ExprAsync,
    site: &TokenStream,
    init: &TokenStream,
    options: &TokenStream,
) -> syn::Result<TokenStream> {
    let generator: bool = scan_yields(|scan: &mut YieldScan| scan.visit_block(&block.block))?;
    let moved: bool = block.capture.is_some();
    let mut body: syn::Block = block.block.clone();
    if generator {
        LowerYield.visit_block_mut(&mut body);
    }
    let (names, uses_self): (Vec<Ident>, bool) = if moved { (Vec::new(), false) } else { analyse_block(&block.block) };
    let clones: Vec<TokenStream> = names.iter().map(capture_clone).collect();
    let self_capture: TokenStream = if uses_self { self_clone() } else { TokenStream::new() };
    let mut body_tokens: TokenStream = body.to_token_stream();
    if uses_self {
        body_tokens = rewrite_self(body_tokens);
    }
    let call: TokenStream = if generator {
        quote! {
            ::lithia::__private::source_gen(
                &__LITHIA_SITE,
                #init,
                move |__lithia_y: ::lithia::__private::Yielder<_>| async move #body_tokens,
                #options,
            )
        }
    } else {
        quote! {
            ::lithia::__private::source_async(&__LITHIA_SITE, #init, async move #body_tokens, #options)
        }
    };
    Ok(quote! {{
        #site
        #(#clones)*
        #self_capture
        #call
    }})
}

fn expand_derived(
    closure: &ExprClosure,
    site: &TokenStream,
    init: &TokenStream,
    options: &TokenStream,
) -> syn::Result<TokenStream> {
    // Dependencies: plain identifiers, each once (spec §4.1).
    let mut params: Vec<Ident> = Vec::new();
    let mut checks: Vec<TokenStream> = Vec::new();
    let mut names: HashSet<String> = HashSet::new();
    for input in &closure.inputs {
        let Some(ident) = param_ident(input) else {
            return Err(syn::Error::new_spanned(input, "dependencies are named by plain identifiers, each once"));
        };
        if !names.insert(ident.to_string()) {
            return Err(syn::Error::new_spanned(input, "dependencies are named by plain identifiers, each once"));
        }
        if let Some(ty) = param_type(input) {
            checks.push(quote_spanned!(ty.span()=> let _: &#ty = &#ident;));
        }
        params.push(ident.clone());
    }
    let count: usize = params.len();
    let deps: TokenStream = quote! {
        let __lithia_deps: [::lithia::__private::NodeHandle; #count] =
            [#(::lithia::__private::node_ref(&#params)),*];
    };

    let is_async: bool = closure.asyncness.is_some();
    let moved: bool = closure.capture.is_some();
    let generator: bool = scan_yields(|scan: &mut YieldScan| scan.visit_expr(&closure.body))?;
    let mut body: Expr = (*closure.body).clone();
    if generator {
        LowerYield.visit_expr_mut(&mut body);
    }

    let (captured, uses_self): (Vec<Ident>, bool) = analyse(&closure.body);
    let mut body_tokens: TokenStream = body.to_token_stream();
    if uses_self && !moved {
        body_tokens = rewrite_self(body_tokens);
    }
    let self_capture: TokenStream = if uses_self && !moved { self_clone() } else { TokenStream::new() };

    if !is_async && !generator {
        // Synchronous body: captures are cloned into the closure, which lives in one Arc.
        let clones: Vec<TokenStream> = if moved { Vec::new() } else { captured.iter().map(capture_clone).collect() };
        let closure_tokens: TokenStream = match &closure.output {
            syn::ReturnType::Type(arrow, ty) => quote!(move || #arrow #ty #body_tokens),
            syn::ReturnType::Default => quote!(move || #body_tokens),
        };
        return Ok(quote! {{
            #site
            #deps
            #(#checks)*
            #(#clones)*
            #self_capture
            ::lithia::__private::derived(&__LITHIA_SITE, &__lithia_deps, #init, #closure_tokens, #options)
        }});
    }

    // Async bodies and generators: captures live in one Arc, cloned once per run.
    let mut bundle: Vec<Ident> = captured.clone();
    if uses_self && !moved {
        bundle.push(format_ident!("__self"));
    }
    let clones: Vec<TokenStream> = if moved { Vec::new() } else { captured.iter().map(capture_clone).collect() };
    let unpacked: TokenStream = unpack(&bundle);
    let call: TokenStream = if generator {
        quote! {
            ::lithia::__private::derived_gen(
                &__LITHIA_SITE,
                &__lithia_deps,
                #init,
                move |__lithia_y: ::lithia::__private::Yielder<_>| {
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
        quote! {
            ::lithia::__private::derived_async(
                &__LITHIA_SITE,
                &__lithia_deps,
                #init,
                move || {
                    let __lithia_caps = ::std::sync::Arc::clone(&__lithia_caps);
                    async move {
                        #unpacked
                        #body_tokens
                    }
                },
                #options,
            )
        }
    };
    Ok(quote! {{
        #site
        #deps
        #(#checks)*
        #(#clones)*
        #self_capture
        let __lithia_caps = ::std::sync::Arc::new((#(#bundle,)*));
        #call
    }})
}
