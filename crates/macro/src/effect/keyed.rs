// SPDX-FileCopyrightText: 2026 lithia Authors
// SPDX-License-Identifier: MPL-2.0

//! The `key` option and the probes a keyed creation compares (spec §4.7).

use super::*;

/// The `key` option: an expression, or a closure whose parameters name variables in scope.
pub(super) enum KeyArg {
    Expr(Expr),
    Closure { params: Vec<Ident>, body: Expr },
}

impl KeyArg {
    pub(super) fn parse(value: &Expr) -> syn::Result<KeyArg> {
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
    pub(super) fn lookup(&self) -> TokenStream {
        quote!(&::lithia::__private::KeyRef::new(&__LITHIA_SITE, &__lithia_key))
    }

    /// The key's value, wrapped where it is written so a key that is not `KeyValue` is named
    /// there once: the expression, or the closure's body with each parameter bound by reference.
    pub(super) fn value(&self) -> TokenStream {
        match self {
            KeyArg::Expr(expr) => quote_spanned!(expr.span()=> ::lithia::__private::NodeKey::new(#expr)),
            KeyArg::Closure { params, body } => {
                let wrap: TokenStream = quote_spanned!(body.span()=> ::lithia::__private::NodeKey::new);
                quote! {{
                    #(
                        #[allow(unused_variables, clippy::shadow_reuse, clippy::shadow_same, clippy::shadow_unrelated)]
                        let #params = &#params;
                    )*
                    #wrap(#body)
                }}
            }
        }
    }
}

/// Binds one probe of a capture, taken by reference before anything is cloned or moved.
pub(super) fn probe_binding(probe: &Ident, value: TokenStream) -> TokenStream {
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
pub(super) fn probes(names: &[Ident], uses_self: bool) -> TokenStream {
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
pub(super) fn lookup_or_create(
    lookup: TokenStream,
    same: TokenStream,
    same_options: &TokenStream,
    create: TokenStream,
) -> TokenStream {
    // The creation is checked first, so the value type is known where the comparator is.
    quote! {
        let __lithia_reclaim = #lookup;
        match __lithia_reclaim {
            __lithia_reclaim @ (::lithia::__private::Reclaim::Changed(_) | ::lithia::__private::Reclaim::Fresh) => {
                let __lithia_inputs: ::lithia::__private::Inputs = ::lithia::__private::Inputs::of(&__lithia_probes);
                #create
            }
            ::lithia::__private::Reclaim::Same(__lithia_node) => __lithia_node.#same(#same_options),
        }
    }
}
