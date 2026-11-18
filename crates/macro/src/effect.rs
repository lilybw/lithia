// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: GPL-2.0-or-later

//! Parsing and expansion of `effect!` (spec §3.1, §3.2, §4).

use std::collections::HashSet;

use proc_macro2::{Span, TokenStream};
use quote::{ToTokens, format_ident, quote, quote_spanned};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::visit_mut::VisitMut;
use syn::{Expr, ExprAsync, ExprClosure, Ident, Pat, Token, Type};

use crate::captures::{Captures, Class, rewrite_self};

const OPTIONS: &[&str] = &["init", "equals", "fragility", "ghost", "flags", "key"];

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

/// The `key` option: an expression, or a closure whose parameters name variables in scope.
enum KeyArg {
    Expr(Expr),
    Closure { params: Vec<Ident>, body: Expr },
}

impl KeyArg {
    fn parse(value: &Expr) -> syn::Result<KeyArg> {
        let Expr::Closure(closure) = value else {
            return Ok(KeyArg::Expr(value.clone()));
        };
        if closure.asyncness.is_some() || closure.capture.is_some() {
            return Err(syn::Error::new_spanned(
                closure,
                "a `key` closure names variables in scope and runs at once; write it without `async` or `move`",
            ));
        }
        let mut params: Vec<Ident> = Vec::new();
        for input in &closure.inputs {
            let Some(ident) = param_ident(input) else {
                return Err(syn::Error::new_spanned(
                    input,
                    "`key` parameters name variables in scope by plain identifiers, each once",
                ));
            };
            if params.contains(ident) {
                return Err(syn::Error::new_spanned(
                    input,
                    "`key` parameters name variables in scope by plain identifiers, each once",
                ));
            }
            params.push(ident.clone());
        }
        Ok(KeyArg::Closure { params, body: (*closure.body).clone() })
    }

    /// The key looked up by reference.
    fn lookup(&self) -> TokenStream {
        quote!(&::lithia::__private::KeyRef::new(&__LITHIA_SITE, &__lithia_key))
    }

    /// The key's value, wrapped where it is written so a key that is not `KeyValue` is named
    /// there once: the expression, or the closure's body with each parameter bound by reference.
    fn value(&self) -> TokenStream {
        match self {
            KeyArg::Expr(expr) => quote_spanned!(expr.span()=> ::lithia::__private::NodeKey::new(#expr)),
            KeyArg::Closure { params, body } => {
                let wrap: TokenStream = quote_spanned!(body.span()=> ::lithia::__private::NodeKey::new);
                quote! {{
                    #(
                        #[allow(unused_variables)]
                        let #params = &#params;
                    )*
                    #wrap(#body)
                }}
            }
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

/// Finds what lets a body's future complete: `return`, `?`, or a `break` out of the loop.
#[derive(Default)]
struct Exits {
    /// Loops entered inside the loop being checked.
    depth: usize,
    /// The checked loop's label.
    label: Option<String>,
    found: bool,
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
fn never_completes(stmts: &[syn::Stmt]) -> bool {
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
fn body_stmts(body: &Expr) -> Vec<syn::Stmt> {
    match body {
        Expr::Block(block) if block.attrs.is_empty() && block.label.is_none() => block.block.stmts.clone(),
        other => vec![syn::Stmt::Expr(other.clone(), None)],
    }
}

/// Wraps a body's future so its output is `ty`, when one is known.
fn typed_future(ty: Option<TokenStream>, future: TokenStream) -> TokenStream {
    match ty {
        Some(ty) => quote!(::lithia::__private::output::<#ty, _>(#future)),
        None => future,
    }
}

/// Validated options, as token streams ready for the expansion.
struct Resolved {
    init: Option<Expr>,
    /// The `Shape` of the node: fragility, ghost policy and flags.
    shape: TokenStream,
    /// `.equals(…)`, or nothing.
    equals: TokenStream,
    key: Option<KeyArg>,
}

fn resolve_options(input: &EffectInput, form: &Form<'_>) -> syn::Result<Resolved> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut init: Option<Expr> = None;
    let mut shape: TokenStream = quote!(::lithia::__private::Shape::new());
    let mut equals: TokenStream = TokenStream::new();
    let mut key: Option<KeyArg> = None;
    for option in &input.options {
        let name: String = option.name.to_string();
        if !OPTIONS.contains(&name.as_str()) {
            return Err(syn::Error::new_spanned(
                &option.name,
                format!("unknown option `{name}`; expected one of init, equals, fragility, ghost, flags, key"),
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
                equals = quote!(.equals(#value));
            }
            "fragility" => {
                if !matches!(form, Form::Derived(_)) {
                    return Err(syn::Error::new_spanned(
                        &option.name,
                        "`fragility` applies to derived nodes only; a source node has no parents",
                    ));
                }
                shape = quote!(#shape.fragility({ #consts #value }));
            }
            "ghost" => {
                shape = quote!(#shape.ghost({ #consts #value }));
            }
            "flags" => {
                shape = quote!(#shape.flags({ #consts #value }));
            }
            "key" => key = Some(KeyArg::parse(value)?),
            _ => unreachable!(),
        }
    }
    Ok(Resolved { init, shape, equals, key })
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

/// Binds `ident` to its capture: an internal clone, or, in a `move` body, the moved value
/// flagged internal. `dependency` names are handles, so their whole capture can be read-only.
fn capture_take(ident: &Ident, class: &Class, moved: bool, dependency: bool) -> TokenStream {
    // A `move` body is analysed by name only.
    let read_only: bool = *class == Class::ReadOnly;
    if let (Class::Paths { all, written }, false, false) = (class, moved, dependency) {
        return path_take(&quote!(#ident), ident, all, written);
    }
    if let (Class::Paths { all, written }, true, false) = (class, moved, dependency) {
        return moved_paths_take(ident, all, written);
    }
    if moved {
        // The body moves the value as written; a handle is flagged internal in place, and a
        // dependency through `Readable`, so generic code is flagged too.
        let flag: TokenStream = match (dependency, read_only) {
            (true, _) => quote_spanned!(ident.span()=> ::lithia::__private::move_dep(&#ident, #read_only)),
            (false, false) => quote_spanned!(ident.span()=> (&#ident).__lithia_move()),
            (false, true) => quote_spanned!(ident.span()=> (&#ident).__lithia_move_ro()),
        };
        return quote_spanned! {ident.span()=>
            __lithia_cap.take(false, || {
                #[allow(unused_imports)]
                use ::lithia::__private::MoveAny as _;
                #flag
            });
        };
    }
    // Any name other than a dependency is read-only only when its type is a handle, which
    // method dispatch decides.
    let session_read_only: bool = read_only && dependency;
    let call: TokenStream = if read_only && !dependency {
        quote_spanned!(ident.span()=> (#ident).__lithia_capture_ro())
    } else {
        quote_spanned!(ident.span()=> (#ident).__lithia_capture())
    };
    quote_spanned! {ident.span()=>
        #[allow(unused_variables)]
        let #ident = __lithia_cap.take(#session_read_only, || {
            #[allow(unused_imports)]
            use ::lithia::__private::CaptureAny as _;
            #call
        });
    }
}

/// Clones a value used only through field paths: when every path is a handle, the clone
/// records no edges and each path that may write records its own.
fn path_take(base: &TokenStream, binding: &Ident, all: &[Vec<Ident>], written: &[Vec<Ident>]) -> TokenStream {
    let checks: Vec<TokenStream> =
        all.iter().map(|fields: &Vec<Ident>| quote!(&& (#base #(.#fields)*).__lithia_is_handle())).collect();
    let edges: Vec<TokenStream> =
        written.iter().map(|fields: &Vec<Ident>| quote!((#base #(.#fields)*).__lithia_field();)).collect();
    quote_spanned! {binding.span()=>
        let __lithia_paths: bool = {
            #[allow(unused_imports)]
            use ::lithia::__private::FieldAny as _;
            true #(#checks)*
        };
        if __lithia_paths {
            #[allow(unused_imports)]
            use ::lithia::__private::FieldAny as _;
            #(#edges)*
        }
        #[allow(unused_variables)]
        let #binding = __lithia_cap.take(__lithia_paths, || {
            #[allow(unused_imports)]
            use ::lithia::__private::CaptureAny as _;
            (#base).__lithia_capture()
        });
    }
}

/// Flags the handles of a moved value used only through field paths: when every path is a
/// handle, each is flagged internal in place, read-only or not by its own uses.
fn moved_paths_take(ident: &Ident, all: &[Vec<Ident>], written: &[Vec<Ident>]) -> TokenStream {
    let checks: Vec<TokenStream> =
        all.iter().map(|fields: &Vec<Ident>| quote!(&& (#ident #(.#fields)*).__lithia_is_handle())).collect();
    let flags: Vec<TokenStream> = all
        .iter()
        .map(|fields: &Vec<Ident>| {
            if written.contains(fields) {
                quote!((&#ident #(.#fields)*).__lithia_move();)
            } else {
                quote!((&#ident #(.#fields)*).__lithia_move_ro();)
            }
        })
        .collect();
    quote_spanned! {ident.span()=>
        let __lithia_paths: bool = {
            #[allow(unused_imports)]
            use ::lithia::__private::FieldAny as _;
            true #(#checks)*
        };
        __lithia_cap.take(false, || {
            #[allow(unused_imports)]
            use ::lithia::__private::MoveAny as _;
            if __lithia_paths {
                #(#flags)*
            } else {
                (&#ident).__lithia_move();
            }
        });
    }
}

fn self_clone(class: &Class) -> TokenStream {
    let binding: Ident = format_ident!("__self");
    if let Class::Paths { all, written } = class {
        return path_take(&quote!(self), &binding, all, written);
    }
    quote! {
        #[allow(unused_variables)]
        let __self = __lithia_cap.take(false, || {
            #[allow(unused_imports)]
            use ::lithia::__private::CaptureAny as _;
            (self).__lithia_capture()
        });
    }
}

/// Opens the capture session an expansion's captures are recorded in.
fn session_begin() -> TokenStream {
    quote!(let __lithia_cap = ::lithia::__private::Capturing::begin();)
}

/// Closes the capture session; what it recorded goes to node creation.
fn session_end() -> TokenStream {
    quote!(let __lithia_captured: ::lithia::__private::Captured = __lithia_cap.finish();)
}

/// Rebinds each capture by reference at the top of a body.
fn by_reference(names: &[Ident]) -> TokenStream {
    quote! {
        #(
            #[allow(unused_variables)]
            let #names = &#names;
        )*
    }
}

/// What a body captures: each name with its class, and `self`'s class if it uses `self`.
struct Analysis {
    names: Vec<Ident>,
    classes: Vec<Class>,
    self_class: Option<Class>,
}

fn analysed(captures: Captures) -> Analysis {
    let classes: Vec<Class> = captures.found.iter().map(|ident: &Ident| captures.class(&ident.to_string())).collect();
    let self_class: Option<Class> = captures.uses_self.then(|| captures.class("self"));
    Analysis { names: captures.found, classes, self_class }
}

fn analyse(body: &Expr) -> Analysis {
    let mut captures: Captures = Captures::new();
    captures.expr(body);
    analysed(captures)
}

fn analyse_block(block: &syn::Block) -> Analysis {
    let mut captures: Captures = Captures::new();
    captures.block(block);
    analysed(captures)
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

/// Binds one probe of a capture, taken by reference before anything is cloned or moved.
fn probe_binding(probe: &Ident, value: TokenStream) -> TokenStream {
    quote! {
        let #probe = {
            #[allow(unused_imports)]
            use ::lithia::__private::{
                CaptureAny as _, ProbeViaArc as _, ProbeViaEq as _, ProbeViaHandle as _, ProbeViaOpaque as _,
            };
            (&&&&::lithia::__private::Probed((#value).__lithia_capture_ref())).__lithia_probe()
        };
    }
}

/// Probes every capture of a keyed body, `self` included, into `__lithia_probes` (spec §4.7).
fn probes(names: &[Ident], uses_self: bool) -> TokenStream {
    let mut bindings: Vec<TokenStream> = Vec::new();
    let mut refs: Vec<Ident> = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let probe: Ident = format_ident!("__lithia_p{}", index);
        bindings.push(probe_binding(&probe, quote!(#name)));
        refs.push(probe);
    }
    if uses_self {
        let probe: Ident = format_ident!("__lithia_p_self");
        bindings.push(probe_binding(&probe, quote!(self)));
        refs.push(probe);
    }
    let count: usize = refs.len();
    quote! {
        #(#bindings)*
        let __lithia_probes: [&dyn ::lithia::__private::Probe; #count] = [#(&#refs),*];
    }
}

/// Wraps a creation in the lookup by key, which `__lithia_shape` joins: an equal reclaimed node is
/// returned with the comparator of `same_options`, otherwise `create` runs (spec §4.7).
fn keyed(lookup: TokenStream, same: TokenStream, same_options: &TokenStream, create: TokenStream) -> TokenStream {
    // The creation is checked first, so the value type is known where the comparator is.
    quote! {
        let __lithia_reuse = #lookup;
        match __lithia_reuse {
            __lithia_reuse @ (::lithia::__private::Reuse::Changed(_) | ::lithia::__private::Reuse::Fresh) => {
                let __lithia_inputs: ::lithia::__private::Inputs = ::lithia::__private::Inputs::of(&__lithia_probes);
                #create
            }
            ::lithia::__private::Reuse::Same(__lithia_node) => __lithia_node.#same(#same_options),
        }
    }
}

/// The options chains of a creation: `create` for a new node, `same` for a reclaimed one; a keyed
/// creation evaluates its shape once, in `bind`, before the lookup.
struct Chains {
    bind: TokenStream,
    create: TokenStream,
    same: TokenStream,
}

impl Chains {
    fn of(resolved: &Resolved) -> Chains {
        let Resolved { shape, equals, key, .. }: &Resolved = resolved;
        match key {
            None => Chains {
                bind: TokenStream::new(),
                create: quote!(::lithia::__private::Options::new(#shape) #equals),
                same: TokenStream::new(),
            },
            Some(_) => {
                let same: TokenStream = quote!(::lithia::__private::Options::new(__lithia_shape) #equals);
                Chains {
                    bind: quote!(let __lithia_shape: ::lithia::__private::Shape = #shape;),
                    create: quote!(#same.keyed(__lithia_reuse, __lithia_key, __lithia_inputs)),
                    same,
                }
            }
        }
    }
}

pub(crate) fn expand(input: EffectInput) -> syn::Result<TokenStream> {
    let form: Form<'_> = input.form()?;
    let resolved: Resolved = resolve_options(&input, &form)?;
    let site: TokenStream = site(&input.tokens);
    let chains: Chains = Chains::of(&resolved);
    let Chains { bind, create: options, same: same_options }: &Chains = &chains;
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
            let body: TokenStream = keyed(
                quote!(::lithia::__private::reuse_source(#key_ref, &__lithia_shape, &__lithia_probe)),
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
    Ok(expansion)
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
    let stmts: &[syn::Stmt] = &body.stmts;
    let mut body_tokens: TokenStream = quote!(#(#stmts)*);
    if uses_self {
        body_tokens = rewrite_self(body_tokens);
    }
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
    let body: TokenStream = keyed(
        quote!(::lithia::__private::reuse_source_async(#key_ref, &__lithia_shape, &__lithia_probes)),
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
        let body: TokenStream = keyed(
            quote!(::lithia::__private::reuse_derived(#key_ref, &__lithia_shape, &__lithia_deps, &__lithia_probes)),
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
    if uses_self && !moved {
        body_tokens = rewrite_self(body_tokens);
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
        quote! {
            ::lithia::__private::derived_gen(
                &__LITHIA_SITE,
                &__lithia_deps,
                &__lithia_captured,
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
